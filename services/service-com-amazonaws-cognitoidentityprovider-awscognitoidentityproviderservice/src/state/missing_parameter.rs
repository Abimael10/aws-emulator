// SPDX-License-Identifier: CC0-1.0

//! The sentence this service refuses a request that left out a parameter one of
//! its operations requires.
//!
//! Amazon Cognito words that refusal the same way wherever the missing value is
//! stated — a member of the request, an `AuthParameters` entry a sign-in flow
//! requires, a `ChallengeResponses` entry a challenge requires — so the rule
//! lives in one place and every operation that reports it reads it from here
//! rather than wording its own.

/// What this service's `InvalidParameterException` says for a parameter one of
/// its operations requires and the request did not carry: fixed text whose only
/// variable part is the NAME of the parameter the caller has to add.
///
/// Nothing else of the request is quoted back — not the operation, not the flow
/// or challenge the parameter belongs to, not the values the request did carry —
/// because the name is the one thing that tells the caller what to send.
#[must_use]
pub fn missing_required_parameter_message(parameter: &str) -> String {
    format!("Missing required parameter {parameter}")
}
