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

/// The wire parameter GROUP EC2 names a member under where the whole refusal is that name:
/// `(input shape, member path, parameter group)`.
///
/// EC2's query validator refuses some absent members by stating the parameter group it reads
/// the value under and NOTHING else — no sentence around it — which is the third of the three
/// answers [`absent_required_member_message`] gives. The group is not a spelling of the
/// member: `ModifyReservedInstancesRequest$TargetConfigurations` is read under the
/// `TargetReservedInstancesConfigurationSet` group, where the model declares the member's
/// `xmlName` as the list's ITEM type
/// (`smithy_query(query="operation",operation="ModifyReservedInstances")`), so the group a
/// member belongs to is recorded here rather than derived.
///
/// Keyed on `(input shape, member path)` — the one key that names a member of ONE operation —
/// because both the group's name and the fact that this surface states it alone are facts of
/// that operation recorded by its callers, exactly as the page-size wordings are
/// ([`RECORDED_PAGE_SIZE_WORDINGS`]). A member not named here keeps the sentence its surface
/// words, so nothing is answered with a group name this service has not recorded.
pub const RECORDED_PARAMETER_GROUP_ONLY_REFUSALS: &[(&str, &str, &str)] = &[(
    "ModifyReservedInstancesRequest",
    "targetConfigurations",
    "TargetReservedInstancesConfigurationSet",
)];

/// The parameter group EC2 answers as the WHOLE message for the absent member at `path` of
/// `input_shape`, or `None` when that member's refusal is a sentence
/// ([`RECORDED_PARAMETER_GROUP_ONLY_REFUSALS`]).
pub fn parameter_group_only_refusal(input_shape: Option<&str>, path: &str) -> Option<&'static str> {
    RECORDED_PARAMETER_GROUP_ONLY_REFUSALS
        .iter()
        .find(|(shape, member, _)| Some(*shape) == input_shape && *member == path)
        .map(|(_, _, parameter)| *parameter)
}

/// The sentence EC2 refuses a request that left out the `@required` member at `path` of
/// `input_shape` with, under the one code it answers that refusal with
/// ([`MISSING_PARAMETER`]).
///
/// Four answers, read off the member and the surface rather than off the request:
///
/// * a required FILTER LIST is [`FILTERS_CANNOT_BE_NULL_OR_EMPTY`], the sentence EC2 states
///   that member's own non-empty rule in, whichever operation carries it;
/// * a member whose refusal EC2 states as the WIRE PARAMETER GROUP ALONE
///   ([`RECORDED_PARAMETER_GROUP_ONLY_REFUSALS`]) is that group's name and nothing else, with
///   no sentence around it;
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
    if let Some(parameter) = parameter_group_only_refusal(input_shape, path) {
        return parameter.to_string();
    }
    if input_shape.is_some_and(|shape| CORE_PARAMETER_VALIDATOR_INPUT_SHAPES.contains(&shape)) {
        return missing_wire_parameter_message(&wire_parameter_name(input_shape, path));
    }
    missing_required_parameter_message(&declared_member_name(path))
}

/// The SENTENCE one API surface of EC2 words a page size outside its published range with.
///
/// One refusal class, several wordings: the surfaces of the original EC2 and VPC objects state
/// the value, the parameter and the bound, where the surfaces added later state the limit or the
/// whole published range. Each variant interpolates only the value the REQUEST sent and the
/// bound the member's own `@range` publishes, so an operation whose range differs is refused
/// with its own numbers; the variants that state the whole range carry it, because the
/// constraint walk reports only the bound that was missed.
#[derive(Debug, Clone, Copy)]
pub enum PageSizeWording {
    /// `Value ( <value> ) for parameter <parameter> is invalid. Expecting a value smaller
    /// <clause> <bound>.` — the sentence EC2's core parameter validator words, which names the
    /// member the way that surface spells it and either admits the bound or excludes it.
    ValueForParameter {
        /// How this surface spells the member: `maxResults` on the wire, or `MaxResults` as the
        /// model declares it.
        parameter: &'static str,
        /// `than` where the sentence excludes the bound, `than or equal to` where it admits it.
        clause: &'static str,
    },
    /// `The requested max results (<value>) is larger than limit (<bound>).` — the sentence
    /// that states the limit the request overshot rather than the member that carried it.
    AgainstLimit,
    /// `MaxResults must be between <minimum> and <maximum>` — the sentence that states the whole
    /// published range, whichever end the value fell outside.
    WithinRange {
        /// The `(minimum, maximum)` of the member's own `@range`
        /// (`smithy_query(query="operation", …)` reports it on `MaxResults`).
        published: (i32, i32),
    },
    /// `The MaxResults parameter cannot be larger than <bound>.` — the sentence that names
    /// the member and states the bound as a limit the parameter may not pass, rather than
    /// stating the value the request sent.
    ParameterAgainstBound,
    /// `The number of requested max results must be between <minimum> and <maximum>.` — the
    /// same rule stated of the COUNT of results, which is how the IPAM surface words it.
    CountWithinRange {
        /// The `(minimum, maximum)` this sentence states, as AWS states them for that surface.
        published: (i32, i32),
    },
    /// `MaxResults must be greater than <bound>.` — the sentence that names the member and the
    /// one bound that was missed.
    ComparedWithBound,
    /// `Value (<value>) for parameter <parameter> is invalid. ` — the PARAMETER layer's own
    /// clause, which names the member and the value and states no reason after the separating
    /// space, exactly as that layer refuses a filter name
    /// ([`invalid_filter_parameter_value`]).
    ParameterLayer {
        /// The member as that layer names it.
        parameter: &'static str,
    },
}

impl PageSizeWording {
    /// This surface's sentence for `value`, refused against `bound` — the bound the constraint
    /// walk reports — on the `outside` end of the published range.
    pub fn message(self, value: &str, bound: &str, outside: PageSizeBound) -> String {
        match self {
            Self::ValueForParameter { parameter, clause } => {
                page_size_message(parameter, clause, value, bound, outside)
            }
            Self::AgainstLimit => format!(
                "The requested max results ({value}) is {relation} than limit ({bound}).",
                relation = outside.relation(),
            ),
            Self::WithinRange {
                published: (minimum, maximum),
            } => format!("MaxResults must be between {minimum} and {maximum}"),
            Self::ParameterAgainstBound => format!(
                "The MaxResults parameter cannot be {relation} than {bound}.",
                relation = outside.relation(),
            ),
            Self::CountWithinRange {
                published: (minimum, maximum),
            } => format!(
                "The number of requested max results must be between {minimum} and {maximum}."
            ),
            Self::ComparedWithBound => format!(
                "MaxResults must be {comparison} than {bound}.",
                comparison = outside.comparison(),
            ),
            Self::ParameterLayer { parameter } => {
                format!("Value ({value}) for parameter {parameter} is invalid. ")
            }
        }
    }
}

/// How EC2's own refusal of a page size OUTSIDE the range ONE operation publishes reads, for
/// the operations whose refusal its callers recorded.
///
/// One refusal class, several recorded answers. `DescribeSubnets` names the member as the model
/// declares it and admits the bound itself ("smaller than or equal to 1000"), while
/// `DescribeSecurityGroups` names it in lowerCamel and excludes it ("smaller than 1000"), the
/// surfaces added later state the limit or the whole published range instead, and the code
/// differs per operation and per BOUND: the paged describes whose refusal was recorded
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
    /// The sentence this operation's surface words that refusal with.
    pub wording: PageSizeWording,
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
        wording: PageSizeWording::ValueForParameter {
            parameter: "maxResults",
            clause: "than",
        },
    },
    RecordedPageSizeRefusal {
        input_shape: "DescribeSubnetsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_PARAMETER_VALUE,
        wording: PageSizeWording::ValueForParameter {
            parameter: "MaxResults",
            clause: "than or equal to",
        },
    },
    RecordedPageSizeRefusal {
        input_shape: "DescribeVpcsRequest",
        below_smallest_code: INVALID_PARAMETER_VALUE,
        above_largest_code: INVALID_PARAMETER_VALUE,
        wording: PageSizeWording::ValueForParameter {
            parameter: "MaxResults",
            clause: "than or equal to",
        },
    },
    // `DescribeTransitGatewayRouteTables` publishes a maximum of 1000 (`MaxResults`
    // `@range` 5..1000) and is refused by the PARAMETER layer, which states no reason after
    // the separating space and names the member as the model declares it; a page size past
    // the ceiling carries the code EC2 uses for a parameter value it cannot use.
    RecordedPageSizeRefusal {
        input_shape: "DescribeTransitGatewayRouteTablesRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_PARAMETER_VALUE,
        wording: PageSizeWording::ParameterLayer {
            parameter: "MaxResults",
        },
    },
    // The managed prefix list describe states the limit the request overshot.
    RecordedPageSizeRefusal {
        input_shape: "DescribeManagedPrefixListsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_MAX_RESULTS,
        wording: PageSizeWording::AgainstLimit,
    },
    // The BYOIP CIDR describe states the bound its `MaxResults` may not pass
    // (`DescribeByoipCidrsMaxResults`, `@range` 1..100), naming the member as the model
    // declares it and the value not at all, under EC2's own [`INVALID_MAX_RESULTS`].
    RecordedPageSizeRefusal {
        input_shape: "DescribeByoipCidrsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_MAX_RESULTS,
        wording: PageSizeWording::ParameterAgainstBound,
    },
    // The Reachability Analyzer describes state the whole range their `MaxResults`
    // publishes (`NetworkInsightsMaxResults`, `@range` 1..100).
    RecordedPageSizeRefusal {
        input_shape: "DescribeNetworkInsightsPathsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_MAX_RESULTS,
        wording: PageSizeWording::WithinRange {
            published: NETWORK_INSIGHTS_PAGE_SIZE_RANGE,
        },
    },
    // The IPAM discovery reads state the same rule of the COUNT of results, with the bounds
    // AWS states in that sentence.
    RecordedPageSizeRefusal {
        input_shape: "GetIpamDiscoveredAccountsRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_MAX_RESULTS,
        wording: PageSizeWording::CountWithinRange {
            published: IPAM_DISCOVERY_STATED_PAGE_SIZE_RANGE,
        },
    },
    // `GetVpnConnectionDeviceTypes` names the member and the one bound that was missed
    // (`GVCDMaxResults`, `@range` 200..1000).
    RecordedPageSizeRefusal {
        input_shape: "GetVpnConnectionDeviceTypesRequest",
        below_smallest_code: INVALID_MAX_RESULTS,
        above_largest_code: INVALID_MAX_RESULTS,
        wording: PageSizeWording::ComparedWithBound,
    },
];

/// The `(minimum, maximum)` page size the Reachability Analyzer describes publish: the
/// `@range` of `NetworkInsightsMaxResults`, the shape their `MaxResults` targets
/// (`smithy_query(query="operation",operation="DescribeNetworkInsightsPaths")`).
///
/// Named here because the sentence those operations word states the whole range while the
/// constraint walk reports only the bound a value missed, and read by the handlers that cap
/// their own page size, so the bound enforced and the bound stated cannot drift apart.
pub const NETWORK_INSIGHTS_PAGE_SIZE_RANGE: (i32, i32) = (1, 100);

/// The `(minimum, maximum)` the IPAM discovery reads STATE in their page-size sentence.
///
/// AWS words that sentence with these bounds, which are not the ones the member's own `@range`
/// publishes (`IpamMaxResults`, 5..1000): the range a request is held to is the model's, and
/// this pair is only what the sentence says. Kept beside the wording it belongs to so the two
/// cannot drift apart.
pub const IPAM_DISCOVERY_STATED_PAGE_SIZE_RANGE: (i32, i32) = (100, 1000);

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

    /// The word the sentences that state the BOUND rather than the value put the value in
    /// relation to it with: [`PageSizeWording::AgainstLimit`], which states the limit the
    /// request overshot, and [`PageSizeWording::ParameterAgainstBound`], which states the
    /// bound the parameter may not pass.
    fn relation(self) -> &'static str {
        match self {
            Self::Smallest => "smaller",
            Self::Largest => "larger",
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

/// The sentence EC2 words a page size outside `input_shape`'s published range with: that
/// operation's own recorded wording, or the one every paged operation whose wording is not
/// recorded answers ([`DEFAULT_PAGE_SIZE_WORDING`]).
pub fn page_size_wording(input_shape: Option<&str>) -> PageSizeWording {
    recorded_page_size_refusal(input_shape).map_or(
        PageSizeWording::ValueForParameter {
            parameter: DEFAULT_PAGE_SIZE_WORDING.0,
            clause: DEFAULT_PAGE_SIZE_WORDING.1,
        },
        |recorded| recorded.wording,
    )
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
    page_size_wording(input_shape).message(value, bound, outside)
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

/// How EC2 refuses an `attribute` parameter naming a value the action cannot act on, where the
/// answer names no value at all: the fixed sentence, end to end.
///
/// The OTHER of the two sentences EC2 carries for this cause, and the one the operations that
/// WRITE an attribute answer: `ModifySnapshotAttribute` and `ResetSnapshotAttribute` were
/// recorded answering exactly this for `productCodes` — "only volume creation permissions can
/// be modified" — where the describe beside them quotes the value it was given
/// ([`unknown_attribute_parameter_value`]). Which of the two an operation answers is a fact of
/// that operation, recorded by its caller; both carry the same `InvalidParameterValue` code.
///
/// Nothing of the request reaches the message, so there is nothing to echo or to bound.
pub fn unrecognized_attribute<E>() -> crate::scaffolded::Scaffolded<E> {
    invalid_parameter_value("Unrecognized attribute.")
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
