// SPDX-License-Identifier: CC0-1.0

//! The published parameter bounds this service has to enforce by hand, and the sentences
//! EC2 refuses a request that violates one with.
//!
//! EC2 models no error shapes and puts no `@range` on most of the members it publishes a
//! valid range for, so both the bound and its refusal live in the handlers. A refusal is
//! part of the contract a caller programs against — a client that reads the limit out of
//! the message, or branches on the code, has to see what the real service says — so the
//! sentence a handler answers is EC2's own for that violation CLASS, carrying the bound
//! that operation publishes, never a description of the limit composed here.
//!
//! One definition per class, so two operations cannot word the same refusal differently.

use super::*;

/// The smallest page an operation that publishes no larger minimum will serve: one item.
/// A page of NONE is not a page, so zero and below is refused whatever else an operation
/// publishes.
pub const SMALLEST_PAGE_SIZE: i32 = 1;

/// The name EC2 reports a `Filters` entry that names no filter at all by.
///
/// An entry carrying values but no `Name` is still an entry the operation cannot use, and
/// the refusal has to say WHICH entry: EC2 names the absent one rather than quoting an
/// empty name, so an operator can see what the request actually sent.
pub const UNNAMED_FILTER: &str = "null";

/// How EC2 refuses a page size below the smallest one an operation publishes: HTTP 400
/// `InvalidParameterValue`, in the sentence [`max_results_below_bound_message`] words —
/// the value the request sent, in spaced parentheses, and the minimum that operation's
/// `MaxResults` publishes.
///
/// The one constructor every hand-enforced page-size floor of this crate answers through,
/// so the code and the wording of that refusal cannot drift between operations. Both
/// variable parts come from the request and from the operation's own published minimum, so
/// an operation whose minimum differs is refused with its own bound rather than with a
/// number copied from another.
pub fn max_results_below_published_minimum<E>(
    value: i32,
    minimum: i32,
) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value(max_results_below_bound_message(
        &value.to_string(),
        &minimum.to_string(),
    ))
}

/// How EC2 states a parameter the request left out: `The request must include the <Member>
/// parameter. Add the required parameter and retry the request.`
///
/// The one sentence EC2's `MissingParameter` carries, whichever member is absent and whichever
/// operation was called: the member is named as the MODEL declares it and the rest is fixed
/// text, so nothing of one run's request is written into the template. Answered both by a
/// handler that resolves an absent member itself ([`missing_required_parameter`]) and by the
/// refusal override for the `@required` members the boundary resolves
/// (`crate::service::parameter_refusals`), so the two cannot word it differently.
pub fn missing_required_parameter_message(member: &str) -> String {
    format!(
        "The request must include the {member} parameter. Add the required parameter and retry \
         the request."
    )
}

/// The code and the words EC2 refuses a page size OUTSIDE the range an operation publishes
/// with, for the operations whose own refusal its callers recorded:
/// `(input shape, error code, parameter spelling, comparison clause)`.
///
/// One refusal class, several recorded answers. `DescribeSubnets` names the member as the model
/// declares it and admits the bound itself ("smaller than or equal to 1000"), while
/// `DescribeSecurityGroups` names it in lowerCamel and excludes it ("smaller than 1000"); both
/// answer [`INVALID_PARAMETER_VALUE`], where the paged describes whose refusal was recorded
/// elsewhere answer EC2's own [`INVALID_MAX_RESULTS`]. WHICH an operation answers is a fact of
/// that operation recorded by its caller and not of the bound, exactly as the two sentences for
/// a page size below the minimum differ ([`max_results_below_bound_message`]) — so it is keyed
/// to the INPUT SHAPE being read, the one key that names a member of one operation.
///
/// An operation NOT named here keeps the code and the sentence every other paged operation
/// answers: [`INVALID_MAX_RESULTS`] with [`DEFAULT_PAGE_SIZE_WORDING`] for a page size it will
/// not page with, either side of its published range.
pub const RECORDED_PAGE_SIZE_WORDINGS: &[(&str, &str, &str, &str)] = &[
    (
        "DescribeSecurityGroupsRequest",
        INVALID_PARAMETER_VALUE,
        "maxResults",
        "than",
    ),
    (
        "DescribeSubnetsRequest",
        INVALID_PARAMETER_VALUE,
        "MaxResults",
        "than or equal to",
    ),
];

/// The `(parameter spelling, comparison clause)` a paged operation whose own wording is not
/// recorded answers: the member in the lowerCamel the rest of this service's `maxResults`
/// sentences name it in ([`max_results_below_bound_message`]), and the bound admitted — the
/// value a request may still ask for, which is what a `@range` bound means.
pub const DEFAULT_PAGE_SIZE_WORDING: (&str, &str) = ("maxResults", "than or equal to");

/// Which bound of the page-size range an operation publishes a value fell outside.
///
/// EC2 words both in one sentence and varies one word between them, so the two are one value
/// here rather than two sentences that could drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSizeBound {
    /// Below the smallest page the operation serves: the sentence asks for a value GREATER
    /// than the bound.
    Smallest,
    /// Above the largest page it serves: the sentence asks for a value SMALLER than it.
    Largest,
}

impl PageSizeBound {
    /// The word EC2's sentence compares the value with.
    fn comparison(self) -> &'static str {
        match self {
            Self::Smallest => "greater",
            Self::Largest => "smaller",
        }
    }
}

/// Whether EC2's own refusal of a page size outside `input_shape`'s published range is
/// recorded, and the code it answers it with — `None` for every operation that answers the
/// code and the sentence every other paged operation does.
pub fn recorded_page_size_code(input_shape: Option<&str>) -> Option<&'static str> {
    RECORDED_PAGE_SIZE_WORDINGS
        .iter()
        .find(|(shape, _, _, _)| Some(*shape) == input_shape)
        .map(|(_, code, _, _)| *code)
}

/// How EC2 states a page size outside the range an operation publishes: the value the request
/// sent, in spaced parentheses, the parameter that carried it and the bound it has to stay
/// inside — "Value ( `<value>` ) for parameter `<parameter>` is invalid. Expecting a value
/// smaller than or equal to `<bound>`."
///
/// `input_shape` is the input being read, which chooses the wording
/// ([`RECORDED_PAGE_SIZE_WORDINGS`]); `None` — a refusal given before any input shape was
/// read — takes the default. Every variable part comes from the request and the model: the
/// value as the caller sent it (already elided by the constraint walk, so echoing it cannot
/// amplify a request into an arbitrarily long response) and the bound off the member's own
/// `@range`, so an operation whose published range differs is refused with its own bound
/// rather than a copied number.
pub fn page_size_outside_range_message(
    input_shape: Option<&str>,
    value: &str,
    bound: &str,
    outside: PageSizeBound,
) -> String {
    let (parameter, clause) = RECORDED_PAGE_SIZE_WORDINGS
        .iter()
        .find(|(shape, _, _, _)| Some(*shape) == input_shape)
        .map_or(DEFAULT_PAGE_SIZE_WORDING, |(_, _, parameter, clause)| {
            (parameter, clause)
        });
    page_size_message(parameter, clause, value, bound, outside)
}

/// The ONE template every page-size sentence of this service is built from, so no two of them
/// can drift apart: only the parameter's spelling, the comparison clause and which bound was
/// missed vary.
pub fn page_size_message(
    parameter: &str,
    clause: &str,
    value: &str,
    bound: &str,
    outside: PageSizeBound,
) -> String {
    format!(
        "Value ( {value} ) for parameter {parameter} is invalid. Expecting a value {comparison} \
         {clause} {bound}.",
        comparison = outside.comparison(),
    )
}

/// How EC2 refuses a page size outside the range an operation publishes: the two bounds of
/// that range and nothing of the request.
///
/// FIXED text apart from the bounds — the sentence states the whole rule rather than the
/// value at fault, so a caller reads the range it has to stay inside off any one refusal.
pub fn max_results_outside_published_range<E>(
    minimum: i32,
    maximum: i32,
) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value(format!(
        "Maximum results allowed are between {minimum} and {maximum}"
    ))
}

/// How EC2 refuses an instance count outside the range the action publishes: the absolute
/// range, stated as the rule, with nothing of the request in it.
///
/// What the account or a Capacity Reservation can currently SATISFY is a different refusal:
/// this one is about the parameter's own published range, so it is answered before any
/// capacity is looked at.
pub fn instance_count_outside_published_range<E>(
    range: &::std::ops::RangeInclusive<i32>,
) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value(format!(
        "The instance count isn't valid. Enter a value between {} and {} instances, \
         and try again",
        range.start(),
        range.end(),
    ))
}

/// How EC2 refuses a request whose `MaxCount` is below its `MinCount`: one fixed sentence,
/// naming neither count.
///
/// The rule is about the two members' ORDER, not about either value, so the sentence names
/// no value — a caller that sent the pair already knows both.
pub fn maximum_instance_count_below_minimum<E>() -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value("Maximum instance count must not be smaller than minimum instance count")
}

/// How EC2 refuses a parameter whose value has to DIFFER from another parameter's: HTTP 400
/// `InvalidParameterValue`, naming the value the request sent, the parameter it sent it for
/// and the parameter whose value it may not repeat.
///
/// FIXED text apart from those three. Both names are the names the REQUEST uses for those
/// members, so an operation whose pair is spelled another way is refused with its own names
/// rather than with a pair written down here, and one definition serves every operation that
/// publishes such a pair — two of them cannot word this refusal differently. The rule is
/// about the request's own members, so a handler answers it before resolving either value
/// against the store. The value is echoed through [`echoed_resource_id`], so naming it cannot
/// amplify a request into an arbitrarily long response.
pub fn parameter_must_differ_from<E>(
    parameter: &str,
    value: &str,
    other_parameter: &str,
) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value(format!(
        "Invalid value {value} for {parameter}. The value of {parameter} needs to be \
         different to {other_parameter}",
        value = echoed_resource_id(value),
    ))
}

/// How EC2 refuses an `attribute` parameter naming a value the action cannot act on: the
/// value the request sent and the fixed sentence that says the service knows no such
/// attribute.
///
/// It does NOT enumerate the attributes the action does accept: a caller reads those from
/// the API reference, and an enumeration composed here would be this service's own
/// description of the rule rather than EC2's answer. One definition for every operation
/// that narrows an `*AttributeName` enum to the subset it serves. The value is echoed
/// through [`echoed_resource_id`], so naming it cannot amplify a request into an
/// arbitrarily long response.
pub fn unknown_attribute_parameter_value<E>(attribute: &str) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value(format!(
        "Value ({}) for parameter attribute is invalid. Unknown attribute.",
        echoed_resource_id(attribute)
    ))
}

/// How EC2 refuses a request that names more than one attribute type at once: the fixed
/// prefix and the attributes the REQUEST named, in the order the request's own members are
/// declared.
///
/// A request carrying two attributes is refused for naming two of them, whichever two they
/// are, so the names are taken from the request rather than from a pair written down here.
pub fn fields_for_multiple_attribute_types<E>(
    attributes: &[&str],
) -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_combination(format!(
        "Fields for multiple attribute types specified: {}",
        attributes.join(", ")
    ))
}
