// SPDX-License-Identifier: CC0-1.0

//! WHICH of this service's operations is serving the request, for the one refusal rule that is
//! a fact about the OPERATION rather than about the constraint.
//!
//! The framework refuses a request that breaks a constraint the model declares BEFORE any
//! handler runs, and asks the service for the words through `Service::override_refusal`. That
//! seam is handed the violations ALONE: one member path, one constraint class and the value
//! that sat there, with no request and no operation. For most of this service's refusal
//! wording that is enough, because the rules are properties of the constraint. One is not:
//! whether a rule group's `Type` is the type the request WRITES, which it must match the
//! submitted document against, or a type to report on, is a fact about the operation — the
//! same member name is both, on different operations ([`super::validation`]'s rule 4).
//!
//! None of the plumbing that closes that gap is this service's: `service_common::restated_refusal`
//! is the fleet's one home for it, and [`SERVED_OPERATION_LAYER`] is its wrapper, handed back
//! from `Service::override_operation`. It serves the modelled operation the generated routing
//! table names — so the framework keeps the whole deserialize → validate → handle → serialize
//! walk and every status, code and word it answers with — with the served operation in scope
//! for the run, and then hands what it rendered to this service's own restatement.
//!
//! This file states only the two facts that are THIS service's: which operations write the
//! rule group their `Type` member names ([`RULE_GROUP_WRITES`]), and that the response it
//! answers with reports a modelled error's text in the member this model declares
//! ([`super::error_message_member`]).

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::restated_refusal::{served_operation, RestatedRefusalOperation};

/// The operations whose `Type` member names the type OF THE RULE GROUP THE REQUEST WRITES,
/// rather than a type to report on.
///
/// The model is what says so: `CreateRuleGroup$Type` and `UpdateRuleGroup$Type` both state
/// "indicates whether the rule group is stateless or stateful" about the rule group the call
/// writes — and both operations refuse a document whose contents the type does not match —
/// while every other operation taking the same member takes it to pick WHICH rule groups to
/// act on or report (`ListRuleGroups$Type` "indicates the general category of the rule group",
/// `DescribeRuleGroup$Type` identifies one beside its name). A type the public API does not
/// accept is therefore the writing operation's own refusal, and elsewhere stays the validation
/// clause the boundary raised.
///
/// Sorted, which [`writes_a_rule_group`] relies on for `binary_search` and
/// `the_rule_group_writes_are_sorted` pins.
const RULE_GROUP_WRITES: &[&str] = &["CreateRuleGroup", "UpdateRuleGroup"];

/// Whether the operation being served writes a rule group ([`RULE_GROUP_WRITES`]).
///
/// `false` outside any served operation — a refusal the router raised before an operation was
/// reached — where no operation's wording applies and the framework's own stands.
pub(crate) fn writes_a_rule_group() -> bool {
    served_operation().is_some_and(|served| RULE_GROUP_WRITES.binary_search(&&*served).is_ok())
}

/// `resp` as this service answers it, for a request the wrapper below served: a modelled error
/// body whose text is in the member this model declares it in.
///
/// The words the refusal seam chose are already in the body it rendered, so nothing is noted
/// and nothing is restated here — only the member the text travels in is this service's to
/// state.
fn with_this_services_envelope(
    _req: &RtRequest,
    resp: RtResponse,
    _noted: Option<String>,
) -> RtResponse {
    super::error_message_member::with_modelled_message_member(resp)
}

/// The layer every modelled operation of this service is served through: the fleet's wrapper,
/// which puts the served operation in scope for the whole run (read back by
/// [`writes_a_rule_group`]) and answers what the run rendered through
/// [`with_this_services_envelope`].
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call, and
/// one value for the whole process because the wrapper holds nothing per service but that one
/// function.
pub(crate) static SERVED_OPERATION_LAYER: RestatedRefusalOperation =
    RestatedRefusalOperation::new(with_this_services_envelope);

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a rule group's `Type` is a type the request WRITES on the create and the update
    /// of a rule group, and a type to report on everywhere else.
    ///
    /// The rows span both writes, the two reads of a rule group that take the same member
    /// beside a name, the listing that filters on it, a write of another resource kind and an
    /// operation that takes no rule-group type at all.
    #[test]
    fn only_a_rule_group_write_names_the_type_it_writes() {
        // (the operation, whether it writes a rule group)
        let rows: &[(&str, bool)] = &[
            ("CreateRuleGroup", true),
            ("UpdateRuleGroup", true),
            ("DescribeRuleGroup", false),
            ("DeleteRuleGroup", false),
            ("ListRuleGroups", false),
            ("CreateFirewallPolicy", false),
            ("DescribeFirewall", false),
        ];

        for (operation, writes) in rows {
            assert_eq!(
                RULE_GROUP_WRITES.binary_search(operation).is_ok(),
                *writes,
                "case {operation:?}: expected writes_a_rule_group = {writes}"
            );
        }
    }

    /// [`RULE_GROUP_WRITES`] is searched by `binary_search`, which a list out of order
    /// answers wrongly for.
    #[test]
    fn the_rule_group_writes_are_sorted() {
        let mut sorted = RULE_GROUP_WRITES.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            RULE_GROUP_WRITES, &sorted[..],
            "expected the operations sorted, got {RULE_GROUP_WRITES:?}"
        );
    }

    /// Outside a served operation nothing of an operation's wording applies, so the writing
    /// reading of a rule-group type is not on.
    #[tokio::test]
    async fn no_served_operation_reads_as_no_rule_group_write() {
        assert!(
            !writes_a_rule_group(),
            "expected no rule-group write outside a served operation"
        );
    }
}
