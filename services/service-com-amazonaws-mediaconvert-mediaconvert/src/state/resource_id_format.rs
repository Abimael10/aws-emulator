// SPDX-License-Identifier: CC0-1.0

//! The FORM of an identifier a request carries, where the service enforces one.
//!
//! Most identifying members of this API are keys the caller chooses (a queue's, a preset's
//! or a job template's `Name`) or references resolved by lookup (a job's id or ARN), and
//! neither has a form to check: a value that identifies nothing is answered by the blank
//! refusal ([`super::blank_member_message`]) or by the lookup that resolves nothing
//! ([`super::not_found_message`]). A member that identifies a resource of ANOTHER system
//! by a published resource id is different: the id has a form, a value of another form
//! identifies nothing in that system, and the service refuses it as a problem in the
//! request naming the member it arrived in and the form that member takes
//! ([`invalid_resource_id_message`]) rather than accepting it and recording a reference
//! nothing could resolve.
//!
//! The one such member of this API is `CreateResourceShare$SupportCaseId` — "AWS Support
//! case identifier" — whose form is [`SUPPORT_CASE_ID_FORMAT`].

/// The form of an AWS Support case identifier, as the service states it in its own
/// refusal of a value that is not one: [`SUPPORT_CASE_ID_DIGITS`] decimal digits and
/// nothing else.
///
/// Carried as the pattern it is so the refusal and the test applied to a value cannot
/// drift from each other: this string is what [`invalid_resource_id_message`] renders, and
/// [`is_support_case_id`] is what it means.
pub const SUPPORT_CASE_ID_FORMAT: &str = r"^\d{15}$";

/// How many digits a support case identifier carries — the count
/// [`SUPPORT_CASE_ID_FORMAT`] states.
const SUPPORT_CASE_ID_DIGITS: usize = 15;

/// Whether `value` is an AWS Support case identifier, i.e. matches
/// [`SUPPORT_CASE_ID_FORMAT`].
///
/// Only ASCII decimal digits count, and exactly as many as the form states: a value with
/// a prefix, a separator, a space, a sign or any other character is not one, and neither
/// is a shorter or a longer run of digits. Byte length is the digit count because every
/// admitted byte is one ASCII digit.
pub fn is_support_case_id(value: &str) -> bool {
    value.len() == SUPPORT_CASE_ID_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// How this service words the refusal of an identifier whose value is not of the form the
/// service enforces for that member.
///
/// The sentence names the REQUEST MEMBER the value arrived in, in the model's wire
/// spelling, and the form that member's resource id takes — never the value the caller
/// sent, which is one request's and no part of the rule. One sentence for every member
/// with a published resource-id form, as [`super::not_found_message`] is the one sentence
/// for every reference that resolves to nothing.
pub fn invalid_resource_id_message(member: &str, format: &str) -> String {
    format!(
        "You specified an invalid value for {member}. The resource ID must match the \
         format '{format}'."
    )
}
