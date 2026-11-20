// SPDX-License-Identifier: CC0-1.0

//! How this service words the refusals its handlers raise, and the identifier forms it
//! accepts for a request's path labels.
//!
//! One concern, so one file: a refusal's words are part of the API — a client asserts
//! on, logs and routes by them — so each sentence is written ONCE here and every
//! handler that refuses builds it from this module. That is also what keeps the words a
//! refusal takes from the request down to the ones the service really states: a sentence
//! below carries a value from the request only where it is the variable part of that
//! sentence, and the not-found sentence has no variable part at all.
//!
//! Five refusal classes live here:
//!
//! * a resource that does not exist: the one fixed sentence [`resource_not_found`] for a
//!   resource INSIDE an instance or in the phone-number inventory, and
//!   [`instance_not_found`] — which names the instance the request addressed — for an
//!   operation whose addressed resource IS the instance;
//! * a parameter whose value is not one the service accepts ([`invalid_parameter`]),
//!   which names the offending member;
//! * a value longer (or shorter) than the maximum (minimum) the model declares for it
//!   ([`value_above_maximum_length`] / [`value_below_minimum_length`]), which names the
//!   parameter and the violated bound;
//! * an instance-plane path label the service cannot read as an identifier
//!   ([`instance_label_refusal`]), which is a blank label — a request that addresses
//!   nothing at all ([`invalid_request`]) — or one longer than the identifier's modelled
//!   `@length` maximum ([`instance_id_is_not_valid`]);
//! * a page size below the minimum its member's modelled `@range` declares
//!   ([`page_size_below_minimum`]), which names the member and that minimum.
//!
//! Beside them is the identifier form a path label must have, which lives on
//! [`super::is_resource_id`] beside the minter it is the companion of. A handler asks
//! THAT before it resolves anything, so a label the service could never have minted is
//! answered as the bad parameter it is rather than as a resource that happens not to be
//! there.

use crate::ops::types::{
    InvalidParameterException, InvalidRequestException, ResourceNotFoundException,
};

/// The `Message` a Connect `ResourceNotFoundException` carries for a resource INSIDE an
/// instance, or in the phone-number inventory.
///
/// Entirely fixed text — it says neither which kind of resource was missing nor which
/// identifier was asked for — so it is the whole requirement for those classes: an unknown
/// queue, flow, hours of operation or storage association, and a phone number this account
/// never claimed, all state it, and none of them quotes the identifier the request carried.
/// The operations whose addressed resource is the INSTANCE ITSELF name that instance instead
/// ([`instance_not_found`]).
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found";

/// Connect's refusal for a request that names a resource this account does not hold —
/// including an identifier that named one until a delete retired it.
///
/// Every operation of this service that could not resolve a resource INSIDE an instance, or a
/// phone number, words its modelled 404 here, whichever resource it was: the two are the same
/// refusal in the same words, so the service has exactly one spelling of it. See
/// [`RESOURCE_NOT_FOUND_MESSAGE`] for why this sentence takes nothing from the request, and
/// [`instance_not_found`] for the refusal of an operation that addresses an instance.
pub fn resource_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(RESOURCE_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// Connect's refusal for an operation whose ADDRESSED RESOURCE IS AN INSTANCE that this
/// account does not hold — including an identifier that named one until a delete retired it:
/// `Instance <instanceId> not found`.
///
/// The resource the request addressed is the instance, so the refusal says WHICH instance was
/// not found: the sentence names the kind and carries one variable part, the `InstanceId` the
/// REQUEST carried, which is the identifier the caller has to correct. An operation that
/// resolved the instance and then failed to resolve a resource INSIDE it reports the generic
/// [`resource_not_found`] instead: there the instance is the container rather than the
/// addressed resource.
pub fn instance_not_found(instance_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!("Instance {instance_id} not found")),
    }
}

/// Connect's refusal for a parameter whose value is not one it accepts: the modelled
/// `InvalidParameterException` (HTTP 400) whose message is `Invalid parameter: ` plus
/// the offending member's name, spelled as the model spells it.
///
/// The value itself is never quoted — only the member is named.
pub fn invalid_parameter(member: &str) -> InvalidParameterException {
    InvalidParameterException {
        message: Some(format!("Invalid parameter: {member}")),
    }
}

/// Connect's refusal for a value longer than the `@length` maximum the model declares
/// for its member: `The value for <parameter> must be less than or equal to <maximum>`.
///
/// `parameter` is the name the service publishes for that member, which is not always
/// the member's own name (see [`QUEUE_NAME_PARAMETER`]); `maximum` is the member's own
/// modelled bound, so the sentence states the limit the request actually broke.
pub fn value_above_maximum_length(parameter: &str, maximum: usize) -> InvalidParameterException {
    InvalidParameterException {
        message: Some(format!(
            "The value for {parameter} must be less than or equal to {maximum}"
        )),
    }
}

/// The other end of the same bound as [`value_above_maximum_length`], in the same
/// sentence shape: `The value for <parameter> must be greater than or equal to
/// <minimum>`.
///
/// Only the maximum end of this sentence is grounded in a recorded response; the
/// minimum's wording is the symmetric form of that template. A later recording of a
/// too-short value is what would correct it, and this is the one place it is written.
pub fn value_below_minimum_length(parameter: &str, minimum: usize) -> InvalidParameterException {
    InvalidParameterException {
        message: Some(format!(
            "The value for {parameter} must be greater than or equal to {minimum}"
        )),
    }
}

/// The `Message` Connect gives a request it cannot read as addressing anything at all —
/// a `@required` path label that is present but holds nothing but whitespace.
///
/// Entirely fixed text: it names neither the label nor the operation, and quotes nothing
/// the request carried.
pub const INVALID_REQUEST_MESSAGE: &str = "Invalid request";

/// The `Message` Connect's instance plane gives an instance identifier it cannot read as
/// one — a label longer than the identifier's modelled `@length` maximum.
///
/// Also entirely fixed text: the rejected identifier is NOT quoted back.
pub const INSTANCE_ID_NOT_VALID_MESSAGE: &str = "Instance id is not valid";

/// The modelled `@length` maximum of `com.amazonaws.connect#InstanceId`, the shape every
/// `InstanceId` member targets
/// (`smithy_query(query="shape",shape="InstanceId")` -> length min 1 max 100).
pub const INSTANCE_ID_MAX_LENGTH: usize = 100;

/// Connect's refusal for a request whose form it cannot act on: the modelled
/// `InvalidRequestException` (HTTP 400) carrying [`INVALID_REQUEST_MESSAGE`].
pub fn invalid_request() -> InvalidRequestException {
    InvalidRequestException {
        message: Some(INVALID_REQUEST_MESSAGE.to_string()),
        reason: None,
    }
}

/// Connect's refusal for an instance identifier it cannot read as one: the modelled
/// `InvalidRequestException` (HTTP 400) carrying [`INSTANCE_ID_NOT_VALID_MESSAGE`].
pub fn instance_id_is_not_valid() -> InvalidRequestException {
    InvalidRequestException {
        message: Some(INSTANCE_ID_NOT_VALID_MESSAGE.to_string()),
        reason: None,
    }
}

/// How an operation whose addressed resource IS an instance refuses the `InstanceId`
/// path label `label`, or `None` for a label it has to resolve against state.
///
/// One function, so every such operation applies ONE rule in ONE order, ahead of any
/// lookup — which is the point: a label that was never valid has to be answered as the
/// client-input fault it is rather than as a resource that happens to be missing, and
/// only a check in front of the lookup can do that.
///
/// The two classes are the two ends of the identifier's own modelled `@length` bound,
/// each in the sentence Connect states it in:
///
/// * a label holding nothing but whitespace carries no identifier at all, so the request
///   addresses nothing and is the malformed request it is ([`invalid_request`]);
/// * a label longer than [`INSTANCE_ID_MAX_LENGTH`] is not an identifier this service
///   could read, whatever the store holds ([`instance_id_is_not_valid`]).
///
/// Anything between those ends is an identifier as far as the model is concerned, so it
/// is RESOLVED — and answered [`instance_not_found`] when nothing holds it, however
/// unlike a minted id it looks. (That is the narrower companion check
/// [`super::is_resource_id`], which the queue plane applies to its own labels in its own
/// wording; the instance plane's recorded refusals are the two above.)
///
/// Both sentences are fixed, so neither refusal carries the label back to the caller.
pub fn instance_label_refusal(label: &str) -> Option<InvalidRequestException> {
    if label.trim().is_empty() {
        return Some(invalid_request());
    }
    if label.chars().count() > INSTANCE_ID_MAX_LENGTH {
        return Some(instance_id_is_not_valid());
    }
    None
}

/// Connect's refusal for a page size below the minimum its member's modelled `@range`
/// declares: `Invalid <member>. <member> should have minimum value of <minimum>.`
///
/// `member` is the page-size member's own name as the model spells it and `minimum` is
/// that member's modelled lower bound, so the sentence states the member the caller sent
/// and the limit the value actually broke. The rejected value itself is not quoted.
pub fn page_size_below_minimum(member: &str, minimum: i32) -> InvalidRequestException {
    InvalidRequestException {
        message: Some(format!(
            "Invalid {member}. {member} should have minimum value of {minimum}."
        )),
        reason: None,
    }
}

/// The parameter name Connect publishes for a queue's `Name` member: the member is
/// `Name`, but the service calls it `QueueName` when it refuses one — so a refusal of
/// that member names `QueueName`, while a refusal of a path label names the label
/// (`InstanceId`, `QueueId`).
pub const QUEUE_NAME_PARAMETER: &str = "QueueName";
/// The modelled `@length` bound of `com.amazonaws.connect#CommonNameLength127`, the
/// shape `CreateQueue`'s `Name` targets
/// (`smithy_query(query="operation",operation="CreateQueue")` -> length min 1 max 127).
pub const QUEUE_NAME_MIN_LENGTH: usize = 1;
/// See [`QUEUE_NAME_MIN_LENGTH`].
pub const QUEUE_NAME_MAX_LENGTH: usize = 127;

#[cfg(test)]
mod tests {
    use super::*;

    /// The not-found sentence for a resource inside an instance is fixed: it is the same
    /// string whatever was asked for. The sentence for an INSTANCE names the instance.
    #[test]
    fn the_not_found_messages_name_only_an_instance() {
        assert_eq!(
            resource_not_found().message.as_deref(),
            Some("Resource not found")
        );
        for instance_id in [
            "11111111-2222-3333-4444-555555555555",
            "support",
            &"i".repeat(INSTANCE_ID_MAX_LENGTH),
        ] {
            let expected = format!("Instance {instance_id} not found");
            let actual = instance_not_found(instance_id).message;
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {instance_id:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A parameter refusal names the member and nothing else.
    #[test]
    fn a_parameter_refusal_names_the_member() {
        for member in ["InstanceId", "QueueId"] {
            let expected = format!("Invalid parameter: {member}");
            let actual = invalid_parameter(member).message;
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "member {member:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A length refusal names the published parameter and the violated bound.
    #[test]
    fn a_length_refusal_names_the_parameter_and_the_bound() {
        let above = value_above_maximum_length(QUEUE_NAME_PARAMETER, QUEUE_NAME_MAX_LENGTH).message;
        assert_eq!(
            above.as_deref(),
            Some("The value for QueueName must be less than or equal to 127"),
            "got {above:?}"
        );
        let below = value_below_minimum_length(QUEUE_NAME_PARAMETER, QUEUE_NAME_MIN_LENGTH).message;
        assert_eq!(
            below.as_deref(),
            Some("The value for QueueName must be greater than or equal to 1"),
            "got {below:?}"
        );
    }

    /// A path label is acceptable exactly when it has the identifier form this service
    /// mints; anything else is refused, including a value the modelled `@length` bound
    /// would have admitted.
    #[test]
    fn a_path_label_is_an_identifier_of_the_minted_form() {
        let id = "11111111-2222-3333-4444-555555555555";
        let cases: Vec<(&str, String, bool)> = vec![
            ("a minted identifier", id.to_string(), true),
            // The form is case-insensitive, so an upper-case label is an identifier —
            // just not one of the lower-case ids this service mints, so it resolves to
            // no resource rather than being refused as a parameter.
            ("upper-case hex", id.to_ascii_uppercase(), true),
            ("empty", String::new(), false),
            ("a name, not an identifier", "support".to_string(), false),
            ("a truncated id", id[..8].to_string(), false),
            (
                "the right groups, a non-hex character",
                "1111111z-2222-3333-4444-555555555555".to_string(),
                false,
            ),
            (
                "the right characters, the wrong groups",
                "11111111-2222-3333-4444555555555555".to_string(),
                false,
            ),
            // Inside the modelled `InstanceId` bound (1..100 characters) and outside it:
            // neither is an identifier.
            ("one character", "a".to_string(), false),
            ("100 characters", "i".repeat(100), false),
            ("101 characters", "i".repeat(101), false),
            (
                "a resource ARN",
                format!("arn:aws:connect:us-east-1:123456789012:instance/{id}/queue/{id}"),
                false,
            ),
        ];
        for (case, label, accepted) in cases {
            let actual = super::super::is_resource_id(&label);
            assert_eq!(
                actual, accepted,
                "case {case:?}: expected accepted={accepted}, got {actual}"
            );
        }
    }

    /// An instance-plane label is placed in one of three classes, in one order: blank
    /// (the malformed request), longer than the identifier's modelled `@length` maximum
    /// (the identifier the service cannot read), or a label it resolves against state.
    #[test]
    fn an_instance_label_is_refused_by_its_class() {
        let id = "11111111-2222-3333-4444-555555555555";
        let cases: Vec<(&str, String, Option<&str>)> = vec![
            ("a minted identifier", id.to_string(), None),
            ("upper-case hex", id.to_ascii_uppercase(), None),
            ("empty", String::new(), Some(INVALID_REQUEST_MESSAGE)),
            ("one space", " ".to_string(), Some(INVALID_REQUEST_MESSAGE)),
            (
                "several spaces",
                "   ".to_string(),
                Some(INVALID_REQUEST_MESSAGE),
            ),
            ("a tab", "\t".to_string(), Some(INVALID_REQUEST_MESSAGE)),
            // Inside the modelled bound, so resolved however unlike a minted id it
            // looks: the instance plane states no refusal for these.
            ("a name, not an identifier", "support".to_string(), None),
            ("a truncated id", id[..8].to_string(), None),
            ("one character, the modelled minimum", "a".to_string(), None),
            (
                "an identifier with whitespace around it",
                format!(" {id} "),
                None,
            ),
            // Both sides of the modelled maximum.
            (
                "the modelled maximum length",
                "i".repeat(INSTANCE_ID_MAX_LENGTH),
                None,
            ),
            (
                "one past the modelled maximum",
                "i".repeat(INSTANCE_ID_MAX_LENGTH + 1),
                Some(INSTANCE_ID_NOT_VALID_MESSAGE),
            ),
            (
                "far past the modelled maximum",
                "i".repeat(INSTANCE_ID_MAX_LENGTH * 3),
                Some(INSTANCE_ID_NOT_VALID_MESSAGE),
            ),
        ];
        for (case, label, expected) in cases {
            let actual = instance_label_refusal(&label).and_then(|refusal| refusal.message);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A page-size refusal names the member the caller sent and the minimum its model
    /// declares, and quotes no value.
    #[test]
    fn a_page_size_refusal_names_the_member_and_the_minimum() {
        for (member, minimum, expected) in [
            (
                "MaxResults",
                1,
                "Invalid MaxResults. MaxResults should have minimum value of 1.",
            ),
            (
                "PageSize",
                5,
                "Invalid PageSize. PageSize should have minimum value of 5.",
            ),
        ] {
            let actual = page_size_below_minimum(member, minimum).message;
            assert_eq!(
                actual.as_deref(),
                Some(expected),
                "member {member:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
