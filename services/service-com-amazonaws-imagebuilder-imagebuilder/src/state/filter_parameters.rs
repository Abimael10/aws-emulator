// SPDX-License-Identifier: CC0-1.0

//! How a listing of this service addresses ONE filter the request carried, and how it reports
//! a filter that left out a member every filter has to carry.
//!
//! A filter is a pair — the member it selects on and the values that member may take
//! (`smithy_query(query="shape",shape="Filter")`: `name` and `values`, neither of which the
//! model can require) — so a filter missing either half names nothing or admits nothing and
//! cannot select at all. The model carries no `@required` on them, so the requirement is this
//! service's own: a request that left one out is reported as the missing-parameter class every
//! such report of this service carries ([`super::MISSING_PARAMETERS_CODE`]), naming the member
//! of that filter the request omitted ([`missing_filter_member_message`]). A listing checks
//! the two in the order the shape declares them, so a filter that left out BOTH is reported for
//! the first.
//!
//! A refusal ABOUT one filter's value, rather than about a member it left out, names the
//! element it was refused for ([`filter_member_parameter`]): a request carrying several filters
//! has nothing else to tell it which one was refused.

/// The member of one filter that names the member it selects on, as the API addresses it.
pub const FILTER_NAME_MEMBER: &str = "name";

/// The member of one filter that carries the values that member may take, as the API
/// addresses it.
pub const FILTER_VALUES_MEMBER: &str = "values";

/// The parameter a refusal about `member` of the filter at `index` of a listing's `filters`
/// names, as the API addresses it — the member that holds the list, the element's zero-based
/// index, and the member of that element the refusal is about.
///
/// The spelling is the one every parameter path of this service takes
/// (`service_common::validation_message::api_parameter_path`), which renders a list element
/// the same way.
#[must_use]
pub fn filter_member_parameter(index: usize, member: &str) -> String {
    format!("{}[{index}].{member}", super::FILTERS_MEMBER)
}

/// Why a listing refuses a filter that left `member` out.
///
/// The missing-parameter sentence this service reports every parameter its model cannot
/// require with ([`super::missing_parameters_message`]), naming the member of the filter the
/// request omitted and nothing else of the request: there is no value to state a requirement
/// about.
#[must_use]
pub fn missing_filter_member_message(member: &str) -> String {
    super::missing_parameters_message(&[member])
}
