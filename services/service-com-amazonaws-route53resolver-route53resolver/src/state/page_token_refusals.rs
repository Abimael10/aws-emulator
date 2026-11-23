// SPDX-License-Identifier: CC0-1.0

//! How a paginated listing refuses a `NextToken` it never issued.
//!
//! A cursor is minted by whoever owns the collection being paged, so the refusal of
//! one that was never minted is THAT component's refusal, and which component it is
//! decides the sentence a caller reads:
//!
//! * a listing over this service's OWN records (the DNS Firewall domain lists, rule
//!   groups, rules and associations, and the Resolvers on an Outpost) is refused by
//!   Route 53 Resolver itself, with its coded "invalid next token" sentence
//!   ([`coded_next_token_validation`]);
//! * the per-VPC firewall configuration listing pages the account's Amazon EC2 VPC
//!   inventory, whose cursors that service mints and validates, so the refusal is
//!   EC2's own `InvalidParameterValue` sentence as the SDK layer that relayed the
//!   call renders it ([`relayed_next_token_validation`]).
//!
//! Both close with the trace clause of the request, like every other refusal of this
//! service ([`traced_message`]), and both travel as the `ValidationException` those
//! listings model: none of them declares `InvalidNextTokenException`
//! (`smithy_query(query="operation",operation="ListFirewallConfigs")`), which is the
//! shape the listings that DO declare it answer through [`invalid_next_token`].

use super::*;

/// The sentence that states a `NextToken` the service did not issue, worded by the
/// component that validates the cursor — the value it was handed, the parameter it
/// was handed as, and what was wrong with it.
///
/// The submitted token is its one variable part, so two tokens give two sentences.
/// Shared by both refusals here and by [`invalid_next_token_parameter`], so the
/// fixed text is written once.
pub fn unissued_next_token_sentence(next_token: &str) -> String {
    format!("Value ( {next_token} ) for parameter NextToken is invalid. The token is invalid.")
}

/// The `ValidationException` a listing over the account's VPC inventory answers for a
/// cursor that inventory never issued.
///
/// The collection is the account's VPCs, which Amazon EC2 owns, so the cursor is
/// resolved by CALLING that service and the refusal that comes back is its own: the
/// sentence EC2 words for the value ([`unissued_next_token_sentence`]), reported the
/// way this service reports every refusal it relays from that inventory
/// ([`relayed_ec2_message`]) and under the error code EC2 refuses a parameter value
/// it will not read with.
pub fn relayed_next_token_validation(next_token: &str, trace_root: &str) -> ValidationException {
    ValidationException {
        message: Some(relayed_ec2_message(
            &unissued_next_token_sentence(next_token),
            EC2_CODE_INVALID_PARAMETER_VALUE,
            trace_root,
        )),
    }
}

/// The sentence Route 53 Resolver words for a `NextToken` it never issued: the
/// service error code for that fault ([`RSLVR_CODE_INVALID_NEXT_TOKEN`]), the fixed
/// text, and the token the request sent double-quoted inside it as its one variable
/// part.
///
/// The ONE spelling of it, so the two shapes that carry this fault — the
/// `InvalidNextTokenException` the listings that model it answer
/// ([`invalid_next_token`]) and the `ValidationException` the listings that model no
/// such shape answer ([`coded_next_token_validation`]) — cannot word it two ways.
pub fn coded_next_token_message(next_token: &str) -> String {
    rslvr_message(
        RSLVR_CODE_INVALID_NEXT_TOKEN,
        &format!(r#"Invalid next token: "{next_token}", invalid version"#),
    )
}

/// The `ValidationException` a listing over this service's own records answers for a
/// cursor it never issued.
///
/// The collection is this service's, so the refusal is this service's: the sentence it
/// words that fault with ([`coded_next_token_message`]), closed with the trace clause
/// of the request. It states the same fault as [`invalid_next_token`] and differs from
/// it only in the shape it travels as, which is the one these listings model.
pub fn coded_next_token_validation(next_token: &str, trace_root: &str) -> ValidationException {
    ValidationException {
        message: Some(traced_message(
            coded_next_token_message(next_token),
            trace_root,
        )),
    }
}
