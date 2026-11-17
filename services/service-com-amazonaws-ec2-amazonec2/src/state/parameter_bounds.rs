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
/// One of the two sentences EC2's `MissingParameter` carries — the one the input validation of
/// the surfaces added to EC2 after it words, naming the member as the MODEL declares it, the
/// rest being fixed text, so nothing of one run's request is written into the template. Answered
/// both by a handler that resolves an absent member itself ([`missing_required_parameter`]) and
/// by the refusal override for the `@required` members the boundary resolves
/// ([`absent_required_member_message`]), so the two cannot word it differently.
pub fn missing_required_parameter_message(member: &str) -> String {
    format!(
        "The request must include the {member} parameter. Add the required parameter and retry \
         the request."
    )
}

/// How EC2's CORE parameter validator states a parameter the request left out: `The request
/// must contain the parameter <parameter>`.
///
/// The other of the two sentences EC2's `MissingParameter` carries, and the one the surfaces of
/// the original EC2 and VPC objects answer. It names the parameter as the ec2Query WIRE spells
/// it — lowerCamel, the spelling `aws_smithy_schema_server::constraints::path` reports a member
/// under — where [`missing_required_parameter_message`] names the modelled member; the rest is
/// fixed text, so nothing of one run's request is written into the template. The sentence every
/// handler of this crate that resolves an absent parameter itself answers, so the two sides of
/// dispatch cannot word one refusal differently.
pub fn missing_wire_parameter_message(parameter: &str) -> String {
    format!("The request must contain the parameter {parameter}")
}

/// How EC2 refuses a request that carried no filter at all for an operation whose `Filters` is
/// `@required`: `Filters cannot be null or empty`.
///
/// Fixed text with no variable part, and about the member rather than about the request's shape:
/// an operation that SELECTS what it reports with filters can report nothing until it is given
/// one, so a filter list that is absent and one that arrives empty are the same mistake and EC2
/// states both this way. Named here so the boundary refusal and the handler's own check on an
/// empty list answer one sentence.
pub const FILTERS_CANNOT_BE_NULL_OR_EMPTY: &str = "Filters cannot be null or empty";

/// The path the boundary's constraint walk reports an absent `Filters` member under: the
/// member's own name in lowerCamel, which is how `aws_smithy_schema_server::constraints::path`
/// spells a member. Named here so the one place that reads a violation path and the one that
/// words the refusal cannot drift apart.
pub const FILTERS_MEMBER_PATH: &str = "filters";

/// The input shapes whose operation EC2 refuses an absent `@required` member on through its CORE
/// parameter validator, which words the refusal [`missing_wire_parameter_message`].
///
/// EC2 answers `MissingParameter` in two sentences, and WHICH of them an operation answers is a
/// fact of the API SURFACE that operation belongs to, recorded by its callers — exactly as the
/// two sentences for a page size outside a published range are
/// ([`RECORDED_PAGE_SIZE_WORDINGS`]). The surfaces of the original EC2 and VPC objects — the
/// network-interface attachment, the subnet, the security-group reference — are refused by the
/// core parameter validator and answer its sentence; the surfaces added later (IPAM, Client VPN,
/// the Transit Gateway attachments, Capacity Manager) are refused by their own input validation
/// and answer [`missing_required_parameter_message`], which is therefore the default.
///
/// Keyed to the INPUT SHAPE because that is the one key that names the members of ONE operation,
/// so EVERY `@required` member of an operation named here is refused in that operation's own
/// sentence rather than only the member some recording happened to leave out.
pub const CORE_PARAMETER_VALIDATOR_INPUT_SHAPES: &[&str] = &[
    "AttachNetworkInterfaceRequest",
    "DetachNetworkInterfaceRequest",
    "CreateSubnetRequest",
    "DeleteSubnetRequest",
    "DescribeSecurityGroupReferencesRequest",
];

/// The parameter EC2 names the security groups a reference report is asked about by.
///
/// `DescribeSecurityGroupReferencesRequest$GroupId` targets the LIST shape `GroupIds`
/// (`smithy_query(query="operation",operation="DescribeSecurityGroupReferences")`), the member
/// itself keeping the singular name the ec2Query wire indexes its entries under. EC2's refusal
/// names the LIST the service reads the ids into, so the parameter is that shape's name in the
/// lowerCamel the core validator spells a parameter in.
pub const SECURITY_GROUP_IDS_PARAMETER: &str = "groupIds";

/// The parameter EC2's core validator names for a required member whose own name is not the name
/// it reads the value under: `(input shape, member path, parameter)`.
///
/// One entry per member whose target shape, rather than the member, is what the refusal names
/// ([`SECURITY_GROUP_IDS_PARAMETER`]). A member not named here is reported by the path the
/// constraint walk gives, which is already the wire's own spelling.
pub const RECORDED_WIRE_PARAMETER_SPELLINGS: &[(&str, &str, &str)] = &[(
    "DescribeSecurityGroupReferencesRequest",
    "groupId",
    SECURITY_GROUP_IDS_PARAMETER,
)];

/// The parameter EC2's core validator names the member at `path` of `input_shape` by: the path
/// the constraint walk reports, unless that member's recorded spelling differs
/// ([`RECORDED_WIRE_PARAMETER_SPELLINGS`]).
pub fn wire_parameter_name(input_shape: Option<&str>, path: &str) -> String {
    RECORDED_WIRE_PARAMETER_SPELLINGS
        .iter()
        .find(|(shape, member, _)| Some(*shape) == input_shape && *member == path)
        .map_or_else(
            || path.to_string(),
            |(_, _, parameter)| (*parameter).to_string(),
        )
}

/// The sentence EC2 refuses a request that left out the `@required` member at `path` of
/// `input_shape` with, under the one code it answers that refusal with
/// ([`MISSING_PARAMETER`]).
///
/// Three answers, read off the member and the surface rather than off the request:
///
/// * a required FILTER LIST is [`FILTERS_CANNOT_BE_NULL_OR_EMPTY`], the sentence EC2 states
///   that member's own non-empty rule in, whichever operation carries it;
/// * an operation of a surface the core parameter validator refuses
///   ([`CORE_PARAMETER_VALIDATOR_INPUT_SHAPES`]) answers
///   [`missing_wire_parameter_message`], naming the parameter as the wire spells it;
/// * every other operation answers [`missing_required_parameter_message`], naming the member as
///   the model declares it.
///
/// `input_shape` is the input being read; `None` — a refusal given before any input shape was
/// read — takes the default, never another request's surface.
pub fn absent_required_member_message(input_shape: Option<&str>, path: &str) -> String {
    if path == FILTERS_MEMBER_PATH {
        return FILTERS_CANNOT_BE_NULL_OR_EMPTY.to_string();
    }
    if input_shape.is_some_and(|shape| CORE_PARAMETER_VALIDATOR_INPUT_SHAPES.contains(&shape)) {
        return missing_wire_parameter_message(&wire_parameter_name(input_shape, path));
    }
    missing_required_parameter_message(&declared_member_name(path))
}

/// How EC2's own refusal of a page size OUTSIDE the range ONE operation publishes reads, for
/// the operations whose refusal its callers recorded.
///
/// One refusal class, several recorded answers. `DescribeSubnets` names the member as the model
/// declares it and admits the bound itself ("smaller than or equal to 1000"), while
/// `DescribeSecurityGroups` names it in lowerCamel and excludes it ("smaller than 1000"), and
/// the code differs per operation and per BOUND: the paged describes whose refusal was recorded
/// elsewhere answer EC2's own [`INVALID_MAX_RESULTS`]. WHICH an operation answers is a fact of
/// that operation recorded by its caller and not of the bound, exactly as the two sentences for
/// a page size below the minimum differ ([`max_results_below_bound_message`]) — so it is keyed
/// to the INPUT SHAPE being read, the one key that names a member of one operation.
#[derive(Debug, Clone, Copy)]
pub struct RecordedPageSizeRefusal {
    /// The input shape whose request this reads the refusal for, as the model names it.
    pub input_shape: &'static str,
    /// The code EC2 answers a page size BELOW the smallest page this operation serves with.
    pub below_smallest_code: &'static str,
    /// The code EC2 answers a page size ABOVE the largest page it serves with.
    pub above_largest_code: &'static str,
    /// How its sentence spells the parameter that carried the page size.
    pub parameter: &'static str,
    /// How that sentence compares the value with the bound — whether the bound itself is
    /// admitted.
    pub clause: &'static str,
}

/// The recorded refusals themselves, one per operation.
///
/// An operation NOT named here keeps the code and the sentence every other paged operation
/// answers: [`INVALID_MAX_RESULTS`] with [`DEFAULT_PAGE_SIZE_WORDING`] for a page size it will
/// not page with, either side of its published range.
pub const RECORDED_PAGE_SIZE_WORDINGS: &[RecordedPageSizeRefusal] = &[
    RecordedPageSizeRefusal {
        input_shape: "DescribeSecurityGroupsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_PARAMETER_VALUE,
        parameter: "maxResults",
        clause: "than",
    },
    RecordedPageSizeRefusal {
        input_shape: "DescribeSubnetsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_PARAMETER_VALUE,
        parameter: "MaxResults",
        clause: "than or equal to",
    },
    RecordedPageSizeRefusal {
        input_shape: "DescribeVpcsRequest",
        below_smallest_code: INVALID_PARAMETER_VALUE,
        above_largest_code: INVALID_PARAMETER_VALUE,
        parameter: "MaxResults",
        clause: "than or equal to",
    },
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
/// recorded, and the code it answers it with on the side of the range the value fell
/// `outside` — `None` for every operation that answers the code and the sentence every other
/// paged operation does.
pub fn recorded_page_size_code(
    input_shape: Option<&str>,
    outside: PageSizeBound,
) -> Option<&'static str> {
    let recorded = recorded_page_size_refusal(input_shape)?;
    Some(match outside {
        PageSizeBound::Smallest => recorded.below_smallest_code,
        PageSizeBound::Largest => recorded.above_largest_code,
    })
}

/// EC2's recorded refusal for `input_shape`, or `None` when that operation's own refusal was
/// not recorded. `None` for the input shape too — a refusal given before any input shape was
/// read — so such a refusal takes the default rather than another request's wording.
fn recorded_page_size_refusal(input_shape: Option<&str>) -> Option<&'static RecordedPageSizeRefusal> {
    RECORDED_PAGE_SIZE_WORDINGS
        .iter()
        .find(|recorded| Some(recorded.input_shape) == input_shape)
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
    let (parameter, clause) = recorded_page_size_refusal(input_shape).map_or(
        DEFAULT_PAGE_SIZE_WORDING,
        |recorded| (recorded.parameter, recorded.clause),
    );
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
