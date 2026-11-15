// SPDX-License-Identifier: CC0-1.0

//! The checks this service makes on a REQUEST before it reads or writes
//! anything, and the words each refusal carries.
//!
//! MediaLive judges a request in its own layer, ahead of the lookup or the
//! store: an idempotency token a create needs, a name a create needs, the
//! syntax of a resource id a path carries, and the IAM role a channel is asked
//! to run as. Those checks belong to no single operation — the same token rule
//! covers every create of one resource family, and the same id grammar covers
//! every path label naming a resource whose ids this service mints as decimal
//! numbers — so they live here, once, and each handler maps the message onto the
//! modelled client error its OWN model declares, exactly as the handlers
//! wrapping [`super::bounded_length`] and [`super::list_page_size`] do.
//!
//! Each refusal is composed from the REQUEST and from this service's own facts,
//! never from a recorded response: the resource family the create mints, the id
//! the path carried, the role ARN the request named.

use super::validation_problem;
use crate::ops::types::ValidationError;

/// The resource family a create belongs to, as the refusals of that family name
/// it: MediaLive words a create's token and name refusals with the noun of the
/// thing being created, so one wording covers every create of that family
/// (`CreateInput` and `CreatePartnerInput` both mint an input).
pub mod request_family {
    /// Every create that mints a channel.
    pub const CHANNEL: &str = "channel";
    /// Every create that mints an input.
    pub const INPUT: &str = "input";
}

/// The message a create refused for carrying no idempotency token carries: the
/// family of resource it would have minted, then the fixed sentence.
///
/// `RequestId` is `@idempotencyToken` — "Unique request ID to be specified. This
/// is needed to prevent retries from creating multiple resources" — so a create
/// that names none cannot be retried safely, and is refused before anything is
/// read or stored rather than minting a second resource per retry. ONE wording,
/// so two creates of the same family cannot word the omission two ways.
pub fn non_empty_request_id_message(family: &str) -> String {
    format!("Non-empty {family} request id required")
}

/// The message a create refused for carrying no name carries: the family of
/// resource it would have minted, then the fixed sentence.
pub fn non_empty_name_message(family: &str) -> String {
    format!("Non-empty {family} name required")
}

/// The token a create was given, when it is one this service can key a retry
/// on — or the message its refusal carries.
///
/// An ABSENT member and an EMPTY one are the same omission to a caller: neither
/// identifies the request, so neither can make a retry safe.
pub fn request_id(family: &str, request_id: Option<&str>) -> Result<String, String> {
    match request_id.map(str::trim).filter(|token| !token.is_empty()) {
        Some(token) => Ok(token.to_string()),
        None => Err(non_empty_request_id_message(family)),
    }
}

/// The name a create was given, when it named one — or the message its refusal
/// carries. An absent member and an empty one are the same omission, for the
/// reason [`request_id`] states.
pub fn resource_name(family: &str, name: Option<&str>) -> Result<String, String> {
    match name.filter(|name| !name.is_empty()) {
        Some(name) => Ok(name.to_string()),
        None => Err(non_empty_name_message(family)),
    }
}

/// The message a request carrying an id that is not a number at all is refused
/// with: FIXED text, naming the grammar rather than the value, because the value
/// is not an id this service could say anything else about.
pub const NON_NUMERIC_ID_MESSAGE: &str = "Id should include only numbers";

/// `Ok` when `id` is spelled the way this service spells every id of the
/// resources it mints out of a counter — one or more decimal digits and nothing
/// else ([`super::ComAmazonawsMedialiveMedialiveState::mint_channel_id`] and the
/// mints beside it) — and the message its refusal carries otherwise.
///
/// A value outside that grammar is not an id this service could ever have handed
/// out, so it names no resource and could not come to name one: it is a
/// MALFORMED request rather than a request for something absent, and is refused
/// as such BEFORE the lookup. The distinction is the caller's to act on — fix the
/// value, rather than treat the resource as gone.
pub fn numeric_id(id: &str) -> Result<(), &'static str> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NON_NUMERIC_ID_MESSAGE);
    }
    Ok(())
}

/// The service principal a channel's `RoleArn` has to let assume it: this
/// service is what runs the channel under that role, in the global spelling a
/// trust policy names a service by.
pub const CHANNEL_SERVICE_PRINCIPAL: &str = "medialive.amazonaws.com";

/// The element path a problem with the role a channel runs as is reported at:
/// `RoleArn`'s `jsonName`, which is how the request spelled the member.
pub const ROLE_ARN: &str = "roleArn";

/// What a caller must fix about a `RoleArn` this service cannot run a channel
/// under: the role ARN the REQUEST named, and what has to be true of it.
///
/// `RoleArn` is "An optional Amazon Resource Name (ARN) of the role to assume
/// when running the Channel", so a channel can only run under a role this
/// account holds and this service is trusted to assume.
pub fn role_not_assumable_message(role_arn: &str) -> String {
    format!(
        "Make sure the role '{role_arn}' exists in the user account and \
         {CHANNEL_SERVICE_PRINCIPAL} is a trusted service"
    )
}

/// The message a write refused for a `RoleArn` that is not the ARN of an IAM
/// role at all carries: the value the request supplied, echoed as it supplied
/// it, because the value itself is what has to change.
pub fn invalid_role_arn_message(role_arn: &str) -> String {
    format!("Invalid Role ARN: {role_arn}")
}

/// `Ok` when `role_arn` is spelled as the ARN of an IAM role — or the message
/// its refusal carries.
///
/// `RoleArn` is "an Amazon Resource Name (ARN) of the role", so a value that is
/// not one names no role at all: it is a MALFORMED member rather than a role
/// this account might or might not hold, and it is refused before IAM is asked
/// anything. The ARN grammar is the crate's one parse
/// ([`::service_common::arn`]), and the role is required of the IAM vendor so a
/// well-formed ARN naming some other kind of resource is refused too. The
/// ACCOUNT is not judged here: whether the role is one this account holds is
/// what asking IAM answers.
pub fn role_arn_syntax(role_arn: &str) -> Result<(), String> {
    let names_a_role = ::service_common::arn::is_resource_arn_of(role_arn, "iam")
        && ::service_common::arn::resource_id_of_type(role_arn, "role")
            .is_some_and(|name| !name.is_empty());
    if names_a_role {
        return Ok(());
    }
    Err(invalid_role_arn_message(role_arn))
}

/// The one `ValidationErrors` entry a channel write refused for its `RoleArn`
/// carries: the member it was found at and what to fix about it.
pub fn role_not_assumable_problem(role_arn: &str) -> ValidationError {
    validation_problem(ROLE_ARN, &role_not_assumable_message(role_arn))
}

/// The `message` an `UnprocessableEntityException` carrying `problem` states:
/// the element path the problem was found at, then the problem itself.
///
/// A client reads the message; the `ValidationErrors` entry beside it is the
/// same fact in machine-readable form, so the two are composed from the ONE
/// entry rather than written twice.
pub fn problem_message(problem: &ValidationError) -> String {
    let path = problem.element_path.as_deref().unwrap_or_default();
    let message = problem.error_message.as_deref().unwrap_or_default();
    if path.is_empty() {
        return message.to_string();
    }
    format!("{path} {message}")
}

/// The message a paginated request asking for no items at all is refused with:
/// FIXED text — it states the floor rather than echoing the value.
pub const MAX_RESULTS_BELOW_FLOOR_MESSAGE: &str =
    "The maximum number of results must be greater than 0.";

/// The message a paginated request whose `NextToken` continues no walk this
/// service handed out is refused with: FIXED text — the token is not a
/// continuation this service could say anything else about, so nothing of it is
/// echoed.
pub const INVALID_NEXT_TOKEN_MESSAGE: &str = "Invalid next token";

/// The token a listing that resumes at an id it MINTED was given, when it is one
/// that listing could have handed out — or the message its refusal carries.
///
/// Such a listing hands out the id the next page starts at, and every id of that
/// kind is rendered the ONE way the mints render it
/// ([`super::minted_number`]): plain decimal digits. So a token spelled any
/// other way continues no walk this service handed out — it is not a
/// continuation at all, and resuming the scan at it would silently serve a page
/// from wherever that value happens to sort. A request naming no token asks for
/// the first page.
pub fn minted_id_token(next_token: Option<&str>) -> Result<Option<&str>, &'static str> {
    match next_token {
        None => Ok(None),
        Some(token) if super::minted_number(token).is_some() => Ok(Some(token)),
        Some(_) => Err(INVALID_NEXT_TOKEN_MESSAGE),
    }
}

/// The message a paginated request asking for more items than one response
/// carries is refused with: the ceiling itself is the only variable part, and it
/// is this service's published one ([`super::MAX_LIST_PAGE_SIZE`]).
pub fn max_results_above_ceiling_message(ceiling: usize) -> String {
    format!("The maximum number of results you can request at once is {ceiling}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every spelling of an id a path label can carry, against the ONE grammar
    /// the mints hand out: decimal digits only, and at least one.
    #[test]
    fn only_decimal_digits_spell_an_id() {
        for (spelling, accepted) in [
            ("1000000", true),
            ("0", true),
            ("0123", true),
            ("", false),
            ("MyMediaLiveChannel", false),
            ("1000000a", false),
            ("-1", false),
            ("+1", false),
            ("10 00", false),
            ("1_000", false),
            ("１０", false),
        ] {
            let got = numeric_id(spelling);
            assert_eq!(
                got.is_ok(),
                accepted,
                "case {spelling:?}: expected accepted={accepted}, got {got:?}"
            );
            if !accepted {
                assert_eq!(
                    got,
                    Err(NON_NUMERIC_ID_MESSAGE),
                    "case {spelling:?}: expected the fixed wording, got {got:?}"
                );
            }
        }
    }

    /// An absent token and an empty one are the same omission; a token with
    /// characters in it is the token.
    #[test]
    fn a_token_is_present_only_when_it_carries_characters() {
        for (case, given, expected) in [
            ("absent", None, None),
            ("empty", Some(""), None),
            ("blank", Some("   "), None),
            ("a token", Some("abc"), Some("abc")),
        ] {
            let got = request_id(request_family::CHANNEL, given);
            match expected {
                Some(token) => assert_eq!(
                    got.as_deref(),
                    Ok(token),
                    "case {case:?}: expected the token {token:?}, got {got:?}"
                ),
                None => assert_eq!(
                    got,
                    Err("Non-empty channel request id required".to_string()),
                    "case {case:?}: expected the token refusal, got {got:?}"
                ),
            }
        }
    }

    /// The family is what the two create refusals name, so one wording serves
    /// every create of that family.
    #[test]
    fn a_create_refusal_names_the_family_it_mints() {
        for (family, token, name) in [
            (
                request_family::CHANNEL,
                "Non-empty channel request id required",
                "Non-empty channel name required",
            ),
            (
                request_family::INPUT,
                "Non-empty input request id required",
                "Non-empty input name required",
            ),
        ] {
            assert_eq!(
                non_empty_request_id_message(family),
                token,
                "case {family:?}: expected {token:?}"
            );
            assert_eq!(
                non_empty_name_message(family),
                name,
                "case {family:?}: expected {name:?}"
            );
        }
    }

    /// The 422's message is its one validation error read out: where, then what.
    #[test]
    fn a_problem_message_states_the_path_then_the_problem() {
        let problem = role_not_assumable_problem("arn:aws:iam::123456789012:role/absent");
        assert_eq!(
            problem_message(&problem),
            "roleArn Make sure the role 'arn:aws:iam::123456789012:role/absent' exists in the \
             user account and medialive.amazonaws.com is a trusted service",
            "expected the path then the problem, got {:?}",
            problem_message(&problem)
        );
    }
}
