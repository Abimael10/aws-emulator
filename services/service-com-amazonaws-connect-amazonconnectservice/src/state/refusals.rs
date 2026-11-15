// SPDX-License-Identifier: CC0-1.0

//! How this service words the refusals its handlers raise, and the identifier forms it
//! accepts for a request's path labels.
//!
//! One concern, so one file: a refusal's words are part of the API — a client asserts
//! on, logs and routes by them — so each sentence is written ONCE here and every
//! handler that refuses builds it from this module. That is also what keeps a refusal
//! from echoing the request back: the sentences below take only what the error class
//! itself carries.
//!
//! Three refusal classes live here:
//!
//! * a resource that does not exist ([`resource_not_found`]), whose message is FIXED —
//!   it names no resource kind and quotes no identifier;
//! * a parameter whose value is not one the service accepts ([`invalid_parameter`]),
//!   which names the offending member;
//! * a value longer (or shorter) than the maximum (minimum) the model declares for it
//!   ([`value_above_maximum_length`] / [`value_below_minimum_length`]), which names the
//!   parameter and the violated bound.
//!
//! Beside them is the identifier form a path label must have, which lives on
//! [`super::is_resource_id`] beside the minter it is the companion of. A handler asks
//! THAT before it resolves anything, so a label the service could never have minted is
//! answered as the bad parameter it is rather than as a resource that happens not to be
//! there.

use crate::ops::types::{InvalidParameterException, ResourceNotFoundException};

/// The `Message` every `ResourceNotFoundException` of this service carries.
///
/// Entirely fixed text: Amazon Connect's not-found refusal does not say which kind of
/// resource was missing and does not quote the identifier that was asked for, so the
/// same sentence answers an unknown instance, an unknown queue and an unknown resource
/// inside an instance.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found";

/// Connect's refusal for a request that names a resource this account does not hold —
/// including an identifier that named one until a delete retired it.
///
/// Every handler of this service builds its modelled 404 here, so the service has
/// exactly one spelling of it. See [`RESOURCE_NOT_FOUND_MESSAGE`] for why the sentence
/// takes nothing from the request.
pub fn resource_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(RESOURCE_NOT_FOUND_MESSAGE.to_string()),
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

    /// The not-found sentence is fixed: it is the same string whatever was asked for.
    #[test]
    fn the_not_found_message_is_fixed_text() {
        assert_eq!(
            resource_not_found().message.as_deref(),
            Some("Resource not found")
        );
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
}
