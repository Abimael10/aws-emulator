// SPDX-License-Identifier: CC0-1.0

//! The wording of every parameter-level and attribute-level refusal this service answers,
//! and the checks that raise them.
//!
//! This service's model shapes an error for an ATTRIBUTE fault — `InvalidAttributeName`,
//! `InvalidAttributeValue` — and one for each batch fault, but a fault in a PARAMETER is
//! carried by two codes it documents without shaping: one for a parameter the call
//! requires and did not get, one for a parameter whose value it does not accept. The
//! error channel owns those two codes and their variants
//! ([`crate::scaffolded::Scaffolded::missing_parameter`] and
//! [`crate::scaffolded::Scaffolded::invalid_parameter_value`]); the TEXT they carry is
//! worded HERE, once, in the templates the service builds from the request, so no handler
//! and no batch entry invents a second wording for the same class of refusal.

use super::*;

/// How a refusal states that `parameter` was absent. The one wording for that class:
/// [`crate::scaffolded::MissingParameter`] builds its payload from it, and a batch entry
/// reporting the condition per entry uses it too.
pub fn missing_parameter_message(parameter: &str) -> String {
    format!("The request must contain the parameter {parameter}.")
}

/// The first member of `required` the request supplied NO VALUE for, or `None` when every
/// one of them carries a value.
///
/// A member the operation requires is a value the call cannot be served without, so a
/// request carrying no text for one supplied that parameter no more than a request leaving
/// it out did: the refusal for it is the absent-parameter one
/// ([`missing_parameter_message`]), answered BEFORE anything the member names is looked up
/// — there is nothing to resolve, so an unresolvable-value refusal would state the wrong
/// fault. The members are given in the order the model declares them, so a request missing
/// two of them is always refused for the same one.
pub fn absent_required_parameter<'a>(required: &[(&'a str, &str)]) -> Option<&'a str> {
    required
        .iter()
        .find(|(_, supplied)| supplied.is_empty())
        .map(|(member, _)| *member)
}

/// How a refusal states that `value`, supplied for `parameter`, is not one this service
/// accepts: the offending value, the parameter it was supplied for, and the rule it
/// broke. `reason` is a whole sentence and carries its own terminal period.
pub fn invalid_parameter_message(value: &str, parameter: &str, reason: &str) -> String {
    format!("Value {value} for parameter {parameter} is invalid. Reason: {reason}")
}

/// The same template for a refusal that does NOT name the value: the one to use where
/// the value is a string whose length is the very thing being refused, so echoing it
/// would hand an unbounded value back to the caller.
pub fn invalid_parameter_message_without_value(parameter: &str, reason: &str) -> String {
    format!(
        "{} Reason: {reason}",
        invalid_parameter_value_message(parameter)
    )
}

/// The same template with no rule stated after it: the refusal of a value this service
/// cannot read as anything at all, where naming the parameter is the whole diagnosis — an
/// identifier it minted and does not recognise, which neither echoes the value back nor has
/// a published value set the caller could repair it against.
pub fn invalid_parameter_value_message(parameter: &str) -> String {
    format!("Value for parameter {parameter} is invalid.")
}

/// The reason a refusal gives for a value outside the range its parameter publishes, as a
/// RANGE: the two bounds the value has to lie between, which is the one thing the caller has
/// to change the request by.
///
/// Amazon SQS words a range refusal in one of two sentences, and which one a parameter is
/// refused with belongs to that parameter: this is the sentence `VisibilityTimeout` and
/// `MaxNumberOfMessages` carry (recorded behaviour — neither member carries a modelled
/// `@range`, so neither the bounds nor the wording come from the model), and the other is
/// [`bounded_parameter_inequality_reason`]. Each caller names the one its own parameter is
/// refused with, so the wording cannot drift from what that parameter answers.
pub fn bounded_parameter_reason(minimum: i64, maximum: i64) -> String {
    format!("Must be between {minimum} and {maximum}, if provided.")
}

/// The same rule stated as the pair of INEQUALITIES the value has to satisfy, which is the
/// sentence `WaitTimeSeconds` is refused with (recorded behaviour, as above).
pub fn bounded_parameter_inequality_reason(minimum: i64, maximum: i64) -> String {
    format!("Must be >= {minimum} and <= {maximum}, if provided.")
}

/// How a refusal states a rule the request broke as a whole rather than through one
/// named parameter. `reason` is a whole sentence and carries its own terminal period.
pub fn invalid_parameters_message(reason: &str) -> String {
    format!("One or more parameters are invalid. Reason: {reason}")
}

/// The same class of fault as [`invalid_parameters_message`], as a batch RESULT ENTRY
/// states it: the entry's own members could not be validated, so the sentence is about
/// the entry rather than about the request.
pub fn unvalidated_parameters_message(reason: &str) -> String {
    format!("One or more parameters cannot be validated. Reason: {reason}")
}

/// Why a message is larger than the queue will carry, as the reason of either sentence
/// above: the bound is the queue's own `MaximumMessageSize`, which applies to ONE message
/// whether it was sent on its own or as one entry of a batch.
pub fn message_too_long_reason(maximum: i64) -> String {
    format!("Message must be shorter than {maximum} bytes.")
}

/// The message system attribute names in the order the model DECLARES them, which is the
/// order a refusal lists the accepted set in.
///
/// Source: `smithy_query(query="shape", shape="MessageSystemAttributeName",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`.
/// It holds exactly the values the generated
/// `MESSAGE_SYSTEM_ATTRIBUTE_NAME_VALUES` carries — membership is still checked against
/// that generated set — but the generated one is sorted, and a published value set is
/// stated in the order it is published.
pub const MESSAGE_SYSTEM_ATTRIBUTE_NAMES: &[&str] = &[
    "All",
    "SenderId",
    "SentTimestamp",
    "ApproximateReceiveCount",
    "ApproximateFirstReceiveTimestamp",
    "SequenceNumber",
    "MessageDeduplicationId",
    "MessageGroupId",
    "AWSTraceHeader",
    "DeadLetterQueueSourceArn",
];

/// How a refusal states that the queue a message movement task names as its source is
/// not one it can move messages out of.
///
/// The sentence states the pre-condition the named source does not meet — the queue has
/// to be the dead-letter queue of another queue — and nothing else: the whole message is
/// fixed text, because the parameter whose value is refused is already named by the
/// parameter-refusal class this is answered as. The documentation sentence of the
/// service's `UnsupportedOperation` class ("This action is currently limited to
/// supporting message redrive from queues that are configured as dead-letter queues
/// (DLQs) ... only") belongs to that OTHER class and is not reused here.
pub const DEAD_LETTER_SOURCE_REQUIRED: &str =
    "Source queue must be configured as a Dead Letter Queue.";

/// How a refusal states that the source queue a message movement task names already has
/// one running: "only one active message movement task is supported per queue at any
/// given time", so the request is refused for the source arn it named rather than for the
/// kind of queue that arn is. Fixed text — the condition clears on its own, and nothing of
/// the request tells the caller anything the sentence does not.
pub const TASK_ALREADY_RUNNING: &str =
    "There is already a task running. Only one active task is allowed for a source queue \
     arn at a given time.";

/// How a refusal states that the message movement task a cancellation named is not one a
/// cancellation can act on: "a message movement can only be cancelled when the current
/// status is RUNNING" (`CancelMessageMoveTask`'s own documentation).
///
/// Fixed text, stating the whole rule: the task's status and its handle are the caller's own
/// and the sentence tells it the one thing it has to know — only a task that is still
/// running can be stopped. A task that has delivered everything it took and one an earlier
/// call already cancelled are refused alike, because the rule is the same for both.
pub const ONLY_ACTIVE_TASKS_CANCELLABLE: &str = "Only active tasks can be cancelled.";

/// How a refusal states that a message movement task was asked to move a queue's messages
/// onto the queue they are already in.
///
/// Fixed text: the two parameters that cannot be equal are the only thing the sentence is
/// about, and the ARN they both carry is the caller's own and tells it nothing it did not
/// send. The rule is a cross-member one the model does not carry — `SourceArn` and
/// `DestinationArn` are plain strings with no modelled relationship — so it is stated here
/// once, where every refusal wording of this service lives.
pub const SAME_SOURCE_AND_DESTINATION: &str =
    "Source queue arn and destination queue arn cannot be the same.";

/// How a refusal states that a message movement task was asked to move the messages of one
/// kind of queue onto the other kind.
///
/// A move carries a queue's messages across unchanged, so the destination has to be able to
/// hold them: only a FIFO queue carries the group and deduplication members a FIFO queue's
/// messages have, and only a standard queue takes a message carrying none. It is the same
/// rule a dead-letter association is held to — "the dead-letter queue of a FIFO queue must
/// also be a FIFO queue. Similarly, the dead-letter queue of a standard queue must also be a
/// standard queue" ([`DEAD_LETTER_TARGET_TYPE_REASON`]) — stated for the pair of queues this
/// call names. Fixed text: the rule is about the two KINDS and the ARNs the caller sent tell
/// it nothing it did not send.
pub const SAME_QUEUE_TYPE_REQUIRED: &str =
    "The source queue and destination queue must be of the same queue type.";

/// The reason a refusal gives for a parameter that does not apply to the KIND of queue
/// the call addresses: a FIFO-only member supplied to a standard queue, or a
/// per-message member a FIFO queue accepts only as a queue attribute.
pub const QUEUE_TYPE_PARAMETER_REASON: &str =
    "The request include parameter that is not valid for this queue type.";

/// How a refusal states that a FIFO send carried no deduplication token and the queue it
/// addresses derives none: "If you don't provide a MessageDeduplicationId and the queue
/// doesn't have ContentBasedDeduplication set, the action fails with an error"
/// (`SendMessageRequest$MessageDeduplicationId`).
pub const NO_DEDUPLICATION_ID: &str = "The queue should either have ContentBasedDeduplication \
                                       enabled or MessageDeduplicationId provided explicitly";

/// How a refusal states that a parameter's value is not one this service accepts, where
/// it names the parameter and nothing else: what that parameter accepts is published
/// with the parameter, not restated per call.
pub fn invalid_value_message(parameter: &str) -> String {
    format!("Invalid value for the parameter {parameter}.")
}

/// The same template with the rule the value broke stated after it, in the `Reason:`
/// clause every refusal of this service that HAS a rule to state carries.
///
/// A value refused for what ANOTHER attribute holds cannot be repaired from the refused
/// attribute's own published value set, so the rule that rejected the pair is the only
/// thing that tells the caller what to change. `reason` is a whole sentence and carries
/// its own terminal period.
pub fn invalid_value_message_with_reason(parameter: &str, reason: &str) -> String {
    format!("{} Reason: {reason}", invalid_value_message(parameter))
}

/// The rule the FIFO throughput limit and the deduplication scope have to satisfy
/// TOGETHER, as the reason a refusal of either half states: "The perMessageGroupId value
/// is allowed only when the value for DeduplicationScope is messageGroup"
/// ([`FIFO_THROUGHPUT_LIMIT`]'s own documentation), stated as the change the caller has
/// to make.
pub fn fifo_throughput_pair_reason() -> String {
    format!(
        "To set {FIFO_THROUGHPUT_LIMIT} to {FIFO_THROUGHPUT_LIMIT_PER_MESSAGE_GROUP}, the \
         {DEDUPLICATION_SCOPE} must be {DEDUPLICATION_SCOPE_MESSAGE_GROUP}."
    )
}

/// Why a retry under a `ReceiveRequestAttemptId` can no longer be served, as the reason
/// of the refusal that turns it away: the token replays its batch only "if none of the
/// messages have been modified (deleted or had their visibility changes)"
/// (`ReceiveMessageRequest$ReceiveRequestAttemptId`), so once one of them was, the
/// receive that token names is not a retryable call any more.
pub const RECEIVE_ATTEMPT_NOT_RETRYABLE: &str = "The receiveRequestAttemptId is not valid \
                                                 anymore since the receive call is not \
                                                 retryable.";

/// The modelled refusal for an attribute value the call does not accept: it names the
/// attribute whose value was refused, which is all this refusal states.
pub fn invalid_attribute_value(attribute: &str) -> InvalidAttributeValue {
    InvalidAttributeValue {
        message: Some(invalid_value_message(attribute)),
    }
}

/// The modelled refusal for an attribute name the call does not accept: it names no
/// attribute of the value set that member carries, or none this call can set on the queue
/// it addresses.
///
/// One wording for every call that takes attribute names — a queue attribute map, a
/// queue attribute name list, or a message system attribute name list — so two
/// operations cannot drift on how they word the same defect. Amazon SQS reports every name
/// it does not HAVE for the call as an UNKNOWN one, whatever made it unacceptable: a name
/// outside the value set, one the service only ever reports, one the queue the call
/// addresses does not have at all — a FIFO-only attribute, or the designator of the FIFO
/// kind, on a standard queue — and one the CALL does not take are all refused in this
/// single sentence, with the name exactly as the request spelled it.
///
/// The one name that is NOT refused this way is the kind designator on a queue that carries
/// it: a FIFO queue reports that attribute, so a later call naming it supplied a name that
/// queue has, and what it asked for is refused as a VALUE
/// ([`QUEUE_TYPE_NOT_MODIFIABLE`], [`validate_attributes`]).
pub fn unknown_attribute_name(name: &str) -> InvalidAttributeName {
    InvalidAttributeName {
        message: Some(format!("Unknown Attribute {name}.")),
    }
}

/// The rule a refusal states for a write that would change what KIND of queue an existing
/// queue is: "You can provide this attribute only during queue creation. You can't change
/// it for an existing queue" ([`FIFO_QUEUE_ATTRIBUTE`]'s own documentation), stated as the
/// change that is not supported, because that is what such a call asked for.
///
/// It is the reason a write of an EXISTING queue that CARRIES the designator gives for an
/// attribute map naming it — this service's own attribute write ([`validate_attributes`]) and
/// a resource-model write alike: the kind is fixed when the queue is made, so the one thing
/// such a call asked for is the one thing no queue admits. A queue that carries no such
/// attribute at all (a standard queue) is answered for the NAME instead, because for that
/// queue the attribute is not one the service has.
pub const QUEUE_TYPE_NOT_MODIFIABLE: &str = "Modifying queue type is not supported.";

/// Why an `Attributes` map was refused. Each handler renders it as its own modelled
/// error variant, so the wire code is the one that operation declares.
#[derive(Debug)]
pub enum AttributeRefusal {
    /// The map named an attribute the call does not have for the queue it addresses: one
    /// that does not exist, one no call sets, one that queue does not carry at all, or one
    /// this call does not take.
    Name(InvalidAttributeName),
    /// The map gave an attribute the call DOES take on that queue a value outside what the
    /// attribute accepts.
    Value(InvalidAttributeValue),
}

/// Check an `Attributes` map a caller supplied.
///
/// `on_create` is true for `CreateQueue`, which is the only call that may supply a
/// `create_only` attribute; `fifo` says whether the queue the map applies to is a FIFO
/// queue, which is what makes the FIFO-only attributes applicable. Names are visited
/// in order, so a map with two bad attributes is always refused for the same one.
///
/// A name this call does not TAKE for the queue it addresses is an UNKNOWN attribute whichever
/// rule put it out of reach ([`unknown_attribute_name`]): an attribute only
/// `GetQueueAttributes` reports, one that applies only to the other kind of queue, the
/// designator of a kind the addressed queue is not ([`AttributeKind::Designator`] — a standard
/// queue carries no attribute saying it is FIFO), and a designator given a value that
/// designates nothing are each simply not an attribute THIS call has for that queue.
///
/// The designator on a queue that DOES carry it — a FIFO queue, which reports it — is refused
/// as a VALUE instead, on every call that is not the create: the name is one that queue has,
/// and what such a map asks for is the one change no queue admits, since the kind is settled
/// when the queue is made ("You can't change it for an existing queue"), so the refusal states
/// that rule ([`QUEUE_TYPE_NOT_MODIFIABLE`]). Every other value of an attribute the call does
/// take is refused as a VALUE too.
pub fn validate_attributes(
    supplied: &HashMap<String, String>,
    on_create: bool,
    fifo: bool,
) -> Result<(), AttributeRefusal> {
    let mut names: Vec<&String> = supplied.keys().collect();
    names.sort();
    for name in names {
        let value = &supplied[name];
        if !QUEUE_ATTRIBUTE_NAME_VALUES.contains(&name.as_str()) {
            return Err(AttributeRefusal::Name(unknown_attribute_name(name)));
        }
        // Reported-but-not-settable: an attribute the queue record derives is one no call
        // has as a settable attribute at all.
        let Some(spec) = settable_attribute(name) else {
            return Err(AttributeRefusal::Name(unknown_attribute_name(name)));
        };
        // An attribute of the FIFO kind of queue on a queue that is not of that kind: a
        // modelled name the queue this call addresses does not have, whether it applies
        // only to FIFO queues or DESIGNATES one — a standard queue carries no attribute
        // saying it is FIFO, which is why a read of one reports none.
        if !applies_to_queue(spec, fifo) {
            return Err(AttributeRefusal::Name(unknown_attribute_name(name)));
        }
        // The attribute that designates the kind of queue, on a call that is not the create
        // and a queue that HAS that attribute: the name is one this queue carries and
        // reports, so what the map got wrong is not the name but the change its value asks
        // for — and the kind of queue is settled when the queue is made. Refused as the
        // VALUE it supplied, with the rule it broke, whatever value that is.
        if !on_create && matches!(spec.kind, AttributeKind::Designator(_)) {
            return Err(AttributeRefusal::Value(InvalidAttributeValue {
                message: Some(invalid_value_message_with_reason(
                    spec.name,
                    QUEUE_TYPE_NOT_MODIFIABLE,
                )),
            }));
        }
        // Create-only on a later call, for an attribute that does not designate the kind of
        // queue: the set of attributes an operation takes is published with that operation,
        // and the attribute map of a call that is not the create lists exactly the settable
        // attributes minus those — so for THIS call the name is not an attribute it has,
        // however the queue it addresses answers a read of it.
        if spec.create_only && !on_create {
            return Err(AttributeRefusal::Name(unknown_attribute_name(name)));
        }
        check_attribute_value(spec, value)?;
    }
    Ok(())
}

/// Apply the attribute map a call supplied to a queue's stored attributes, with the two
/// rules that are about the VALUE rather than about the attribute.
///
/// * An attribute whose form admits text of any length is REMOVED by a value holding no
///   text ("You can remove the ... attribute by setting it to an empty string"), so a
///   cleared policy leaves the queue carrying no `Policy` at all rather than one that is
///   not a document.
/// * A `Policy` is stored in its normalised form
///   ([`authorization::normalised_policy`]), so every later read reports the principals
///   in the spelling AWS canonicalises them to.
/// * A redrive policy attribute is stored in its canonical rendering
///   ([`canonical_redrive_document`]), so every later read reports one document whatever
///   spacing, member order or quoting the caller wrote it with.
///
/// Both `CreateQueue` and `SetQueueAttributes` store through this, so the two cannot
/// disagree about what a supplied value means.
pub fn apply_attributes<'a>(
    attributes: &mut BTreeMap<String, String>,
    supplied: impl IntoIterator<Item = (&'a String, &'a String)>,
) {
    for (name, value) in supplied {
        match stored_attribute(name, value) {
            Some(stored) => attributes.insert(name.clone(), stored),
            None => attributes.remove(name),
        };
    }
}

/// One value as an attribute's wire text carries it: a string stays itself, and every other
/// JSON form is its own compact JSON (`30`, `true`, `{"maxReceiveCount":3}`).
///
/// An `Attributes` value is TEXT on this service's wire, while the same value is typed
/// wherever a resource model declares it — an `AWS::SQS::Queue`'s `VisibilityTimeout` is a
/// number and its `RedrivePolicy` an object. Several services need that conversion, so the
/// rule itself is stated once in the shared crate
/// ([`::service_common::json_wire_values::value_text`]) and named here, beside the attribute
/// vocabulary it belongs to, for the callers that hand this service an attribute map.
pub fn attribute_text(value: &::serde_json::Value) -> String {
    ::service_common::json_wire_values::value_text(value)
}

/// The form attribute `name` is STORED in when a call supplies `value` for it, or `None`
/// when that value removes the attribute. See [`apply_attributes`].
pub fn stored_attribute(name: &str, value: &str) -> Option<String> {
    let removable = settable_attribute(name)
        .is_some_and(|spec| matches!(spec.kind, AttributeKind::Json | AttributeKind::Text));
    if value.is_empty() && removable {
        return None;
    }
    if name == POLICY {
        return Some(authorization::normalised_policy(value));
    }
    if let Some((_, numeric_members)) = CANONICAL_JSON_ATTRIBUTES
        .iter()
        .find(|(attribute, _)| *attribute == name)
    {
        return Some(canonical_redrive_document(value, numeric_members));
    }
    Some(value.to_string())
}

/// Why the attribute set a call would LEAVE a queue carrying cannot hold together, or
/// `None` when it can.
///
/// Some attributes are each individually valid yet constrain one another, so the pair
/// can only be judged over the resulting set rather than over the map one call supplies:
/// the per-message-group FIFO throughput limit "is allowed only when the value for
/// DeduplicationScope is messageGroup".
pub fn attribute_combination_refusal(
    attributes: &BTreeMap<String, String>,
) -> Option<InvalidAttributeValue> {
    let per_message_group = attributes
        .get(FIFO_THROUGHPUT_LIMIT)
        .is_some_and(|value| value == FIFO_THROUGHPUT_LIMIT_PER_MESSAGE_GROUP);
    let message_group_scope = attributes
        .get(DEDUPLICATION_SCOPE)
        .is_some_and(|value| value == DEDUPLICATION_SCOPE_MESSAGE_GROUP);
    (per_message_group && !message_group_scope).then(|| InvalidAttributeValue {
        // The fixed text Amazon SQS words a refused attribute value in, instantiated with
        // the attribute whose value the pair rule rejects, and carrying the `Reason:`
        // clause stating that rule — without it the refusal names no way to repair the
        // request, because the value is outside nothing the named attribute itself
        // publishes. The attribute named is the one the rule is stated ON, whichever half
        // of the pair the call supplied: it is the value that is only conditionally
        // allowed, and the scope is what has to change for it.
        message: Some(invalid_value_message_with_reason(
            FIFO_THROUGHPUT_LIMIT,
            &fifo_throughput_pair_reason(),
        )),
    })
}

/// Check one attribute value against what its own documentation says it accepts.
fn check_attribute_value(spec: &AttributeSpec, value: &str) -> Result<(), AttributeRefusal> {
    // An attribute whose form admits text of any length is also the kind a caller REMOVES
    // by supplying no text for it: "You can remove the ... attribute by setting it to an
    // empty string", so the empty value is accepted and stored as the attribute's absence
    // ([`stored_attribute`]) rather than refused as a malformed document.
    if value.is_empty() && matches!(spec.kind, AttributeKind::Json | AttributeKind::Text) {
        return Ok(());
    }
    // Every attribute value is stored for the queue's lifetime, so the two kinds whose
    // form admits text of any length are bounded FIRST: a caller looping on
    // `CreateQueue`/`SetQueueAttributes` must not be able to grow this store without
    // end (see "Store bounds" above). The ceiling is the one AWS publishes for the
    // largest of them — "The policy size is limited to 8,192 bytes" (the `Policy`
    // attribute's own documentation) — applied to each of them.
    let bounded = match spec.kind {
        AttributeKind::Json | AttributeKind::Text => value.len() <= MAX_ATTRIBUTE_VALUE_BYTES,
        _ => true,
    };
    if !bounded {
        return Err(AttributeRefusal::Value(invalid_attribute_value(spec.name)));
    }
    let accepted = match spec.kind {
        AttributeKind::Integer { min, max } => match value.parse::<i64>() {
            Ok(parsed) => (min..=max).contains(&parsed),
            Err(_) => false,
        },
        AttributeKind::Boolean => value == "true" || value == "false",
        AttributeKind::OneOf(allowed) => allowed.contains(&value),
        AttributeKind::Json => ::serde_json::from_str::<::serde_json::Value>(value).is_ok(),
        AttributeKind::Text => true,
        AttributeKind::Designator(designating) => value == designating,
    };
    if accepted {
        return Ok(());
    }
    // The refusal names the attribute and nothing else: what that attribute accepts is
    // published with the attribute, not restated per call.
    Err(match spec.kind {
        // A designator carries the one value that designates and nothing else, so a value
        // that designates nothing is not a value of this attribute to repair: the
        // attribute the request named is one the service does not have for it, and that is
        // what the refusal states.
        AttributeKind::Designator(_) => AttributeRefusal::Name(unknown_attribute_name(spec.name)),
        // A document attribute whose own documentation defines the document MEMBER BY
        // MEMBER states, in the `Reason:` clause, that the value is not the JSON map that
        // documentation gives it ([`document_form_reason`]): the attribute publishes no
        // value set a caller could repair a malformed document against, so the clause is
        // the only thing that says what was wrong with it. A document this service does
        // not define member by member ("a valid Amazon Web Services policy") has no such
        // sentence and is refused by name alone.
        AttributeKind::Json => AttributeRefusal::Value(match document_form_reason(spec.name) {
            Some(reason) => InvalidAttributeValue {
                message: Some(invalid_value_message_with_reason(spec.name, reason)),
            },
            None => invalid_attribute_value(spec.name),
        }),
        _ => AttributeRefusal::Value(invalid_attribute_value(spec.name)),
    })
}
