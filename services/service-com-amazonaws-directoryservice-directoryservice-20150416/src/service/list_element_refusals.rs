// SPDX-License-Identifier: CC0-1.0

//! A constraint a LIST's ELEMENTS break, stated the way this service states it: at the list
//! member, quoting the list the request sent.
//!
//! ## The rule
//!
//! The framework's constraint walk reports such a violation at the ELEMENT it found it on —
//! `'<list>.<1-based position>.member'`, quoting that one element — because that is where the
//! value is. Directory Service reports it at the list MEMBER: the subject is the WHOLE list
//! that member held, rendered the way a list renders and in the order the request sent it,
//! and the requirement is the element shape's own constraint set nested inside
//! `Member must satisfy constraint: [ … ]`. So a client that surfaces the failing member path
//! to a user names the member it submitted, with the list it submitted beside it, and a list
//! with two offending entries is ONE validation error rather than two. A list inside a
//! structure the request nests is named the same way, by the dotted path of the member that
//! holds it (`vpcSettings.subnetIds`) and with no element position in it either.
//!
//! That is one rule about every list member of every operation, not about one filter, so it is
//! applied here — once, over the violations the framework's own walk found — rather than in
//! any handler: nothing below re-checks anything, and a request the framework would accept is
//! never refused by this.
//!
//! The grouping, the nesting and the envelope are the tree's shared ones
//! ([`::service_common::validation_message::aws_validation_refusal_over_elements_stating`]);
//! what is this service's own is the ORDER its own validator lists an enum's value set in
//! ([`super::stated_enum_values`] states the same order for a clause about a plain member),
//! which list the clause quotes ([`quotable_lists`]), and that the nested set closes
//! with the requirement that the MEMBER be carried where the model never leaves it optional
//! ([`element_set_in_the_services_own_order`] over [`super::stated_element_sets`]).
//!
//! WHY THE WHOLE LIST AND NOT THE OFFENDING ENTRIES. A recorded refusal of a ONE-entry list
//! reads the same either way, which is why this module first stated the offending entries:
//! the only ds recording it was written against was of a one-entry `DnsIpAddrs`. A recorded
//! refusal of a create whose nested `vpcSettings.subnetIds` carried a well-formed subnet id
//! BESIDE an ill-formed one settles it — the clause quotes both, in request order — so the
//! value half is the member's own value rather than a selection from it, which is also what
//! `cloudformation`'s copy of this rule states. That is one rule about every list member of
//! every operation of this service, so a clause here quotes the list the request sent
//! whatever member carried it and however many of its entries offend. The dispatch half of
//! this module is still the second copy of `cloudformation`'s, and both are for
//! `services/common` to own.
//!
//! ## Where it is applied
//!
//! In the operation wrapper every routable request of this service passes through
//! ([`super::request_id_header::RequestIdHeaderOperation`]), because the collecting half needs
//! the REQUEST: `Service::override_refusal` is handed violations whose echoed values the walk
//! ELIDED, and a clause stated at a list member quotes those values. The cost is that a
//! request whose operation declares such a list is read twice — once here and once in the
//! operation's own sequence — which `services/common/src/validation.rs` records as the core
//! seam this re-read exists only until.

use std::collections::{HashMap, HashSet};

use ::aws_smithy_runtime_api::http::Request as RtRequest;
use ::aws_smithy_schema::Schema;
use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use ::service_common::validation::{list_of_element, refusing_violations_with_lists};
use ::service_common::validation_message::{
    aws_validation_refusal_over_elements_stating, element_constraint_set, element_set_requirement,
    requirement,
};
use ::smithy_server_runtime::{Operation, Refusal, Refused, ServerHttpProtocol};

use super::VALIDATION_EXCEPTION;

/// How one operation's refusing violations and submitted lists are collected: the framework's
/// own walk over that operation's input schema, which is why a collector is needed per
/// operation rather than one for the service.
type Collect = fn(&dyn ServerHttpProtocol, &RtRequest) -> Option<(Vec<Violation>, Refused)>;

/// The collector for each operation this module states the refusal of, resolved by the
/// operation's own wire name ([`Operation::NAME`], so no name is spelled twice).
macro_rules! collected_per_operation {
    ( $( $module:ident :: $operation:ident ),* $(,)? ) => {
        /// The collector for the operation `key` names, or `None` for one whose input declares
        /// no list of scalar values of its own — the members a clause of this class can be
        /// about at all. An operation with no such list has no list element to report a
        /// constraint at, so it is not read twice for nothing.
        fn collector(key: &str) -> Option<Collect> {
            $(
                if key == <crate::ops::$module::$operation<()> as Operation>::NAME {
                    return Some(stated_from_request::<crate::ops::$module::$operation<()>>);
                }
            )*
            None
        }
    };
}

// Every operation of this service whose input declares a list of scalars — its own member, or
// one inside a structure that input nests (a create's VPC settings, an AD Connector's connect
// settings, a RADIUS configuration, an assessment's configuration). The operation TYPE is
// named with a placeholder engine, because nothing here constructs one: only the operation's
// name and its input schema are read, and both are associated constants of the type.
collected_per_operation!(
    add_region::AddRegion,
    connect_directory::ConnectDirectory,
    create_conditional_forwarder::CreateConditionalForwarder,
    create_directory::CreateDirectory,
    create_microsoft_ad::CreateMicrosoftAD,
    create_trust::CreateTrust,
    describe_conditional_forwarders::DescribeConditionalForwarders,
    describe_directories::DescribeDirectories,
    describe_domain_controllers::DescribeDomainControllers,
    describe_event_topics::DescribeEventTopics,
    describe_shared_directories::DescribeSharedDirectories,
    describe_snapshots::DescribeSnapshots,
    describe_trusts::DescribeTrusts,
    enable_radius::EnableRadius,
    remove_ip_routes::RemoveIpRoutes,
    remove_tags_from_resource::RemoveTagsFromResource,
    start_ad_assessment::StartADAssessment,
    update_conditional_forwarder::UpdateConditionalForwarder,
    update_directory_setup::UpdateDirectorySetup,
    update_hybrid_ad::UpdateHybridAD,
    update_radius::UpdateRadius,
);

/// The structures this service's operation inputs NEST that declare a list of scalars of
/// their own.
///
/// What they are read for: the element shape's constraint set is looked up by the member a
/// clause is stated at, and codegen attaches a collection member's element constraints to that
/// member's own schema — which, for a member of a nested structure, lives on THAT structure's
/// schema rather than on the operation input's. A member several of these declare with the
/// same element set reads the same either way, and one they declare differently resolves to
/// nothing, which leaves the framework's own clause.
const NESTED_SHAPES: &[&Schema] = {
    use crate::ops::types;

    &[
        // A create's and an add's VPC placement, and an AD Connector's connect settings.
        types::DIRECTORY_VPC_SETTINGS_SCHEMA,
        types::DIRECTORY_CONNECT_SETTINGS_SCHEMA,
        // A directory's RADIUS configuration.
        types::RADIUS_SETTINGS_SCHEMA,
        // An assessment's configuration, and the self-managed instances a hybrid directory's
        // update names.
        types::ASSESSMENT_CONFIGURATION_SCHEMA,
        types::HYBRID_CUSTOMER_INSTANCES_SETTINGS_SCHEMA,
        // The network half of a directory setup update.
        types::NETWORK_UPDATE_SETTINGS_SCHEMA,
    ]
};

/// The refusal this service states for `req` under the operation `O`, or `None` when the
/// request breaks no constraint of a list's ELEMENTS — which leaves the framework's own answer,
/// and this service's other wording of it, exactly as they are.
fn stated_from_request<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<(Vec<Violation>, Refused)> {
    // The violations AND the lists the request carried, out of ONE walk: the value half of a
    // clause stated at a list member is that member's own value, which no violation carries.
    let (violations, submitted) = refusing_violations_with_lists::<O>(protocol, req);
    // Only this class is reworded here. Everything else — an absent required member, a bound
    // on a plain member, a value outside an enum a plain member targets — keeps the answer it
    // has today, including this service's own enum-order wording.
    if !violations
        .iter()
        .any(|violation| list_of_element(&violation.path).is_some())
    {
        return None;
    }
    let quoted = quotable_lists(&violations, submitted);
    let restated: Vec<Violation> = super::in_stated_clause_order(&violations)
        .into_iter()
        .map(super::stated_violation)
        .collect();
    let stated = aws_validation_refusal_over_elements_stating(
        &Refusal::Invalid(restated),
        // The expression a clause quotes is the one the model declares, anchors and all, as
        // `super::stated_violation` left it: the rendering here leaves it as it is, since
        // this module changes which MEMBER a clause is stated at and nothing about how any
        // requirement reads.
        |pattern| pattern.to_string(),
        // The value a clause stated at a list member quotes: the whole list that member
        // held, in the order the request sent it.
        |collection| quoted.get(collection).cloned(),
        // ...and the requirement it nests, with an enum's value set in the order this service
        // lists it.
        |collection| element_set_in_the_services_own_order(O::INPUT_SCHEMA, collection),
    )?;
    Some((
        violations,
        Refused {
            // The code a client branches on for a refused value, restated because a chosen
            // refusal carries both halves.
            code: VALIDATION_EXCEPTION.to_string(),
            message: stated.message,
        },
    ))
}

/// The list each clause may quote: `submitted`, the lists the request carried rendered the way
/// AWS quotes a list-valued member (`[<entry>, <entry>]`,
/// [`::service_common::validation::submitted_lists`]), minus every one this refusal is not
/// allowed to report.
///
/// The clause's value half is the WHOLE submitted list, in the order the request sent it: it
/// is the value that member held, which is what tells a caller which list it submitted and
/// where in it the entries it has to fix sit. The collecting walk owns the rendering and every
/// bound on it, and offers a list only when it collected that list in FULL, so a clause can
/// never quote a list the caller did not send.
///
/// A collection one of whose violations carries NO value (a `@sensitive` element, whose value
/// the validating walk withholds) is dropped here, so no clause can quote a value the refusal
/// is not allowed to report; such a clause states the member alone. BOUNDED by the violation
/// count the walk itself bounds ([`::service_common::validation::MAX_VIOLATIONS`]).
fn quotable_lists(
    violations: &[Violation],
    submitted: HashMap<String, String>,
) -> HashMap<String, String> {
    let withheld: HashSet<&str> = violations
        .iter()
        .filter(|violation| violation.value.is_none())
        .filter_map(|violation| list_of_element(&violation.path))
        .collect();
    submitted
        .into_iter()
        .filter(|(collection, _)| !withheld.contains(collection.as_str()))
        .collect()
}

/// The requirement a clause stated at `collection` nests: the element shape's whole declared
/// constraint set, with an enum's value set listed in the order this SERVICE lists it
/// ([`super::stated_enum_values`]), closing with the requirement that the MEMBER be carried
/// where the model never leaves it optional ([`super::stated_element_sets`]).
///
/// The constraints themselves are the ones the shared reading of the schema produces
/// ([`element_constraint_set`]) — the model's, constraint for constraint — and what this
/// service adds to them is the ORDER an enum's values are listed in and the closing
/// not-null requirement a member the model always carries brings with it. A collection the
/// operation's input
/// declares ITSELF is looked up by its whole path; one inside a structure that input nests is
/// looked up by its OWN member name on the structures this service nests ([`NESTED_SHAPES`]),
/// since that is the schema codegen attached its element constraints to. `None` where no
/// handed-over shape declares a constraint on that member's elements, which leaves the clause
/// the framework's own.
///
/// A `@pattern` inside the NESTED set keeps the expression this service publishes for that
/// shape — the same rendering a per-member clause quotes ([`super::stated_violation`]),
/// which for every shape a recorded element clause of this service covers (the one-entry
/// `DnsIpAddrs` refusal the module header quotes, pinned row by row in
/// `tests/all/refusal_classification_rules.rs`) is the model's text, anchors and all.
fn element_set_in_the_services_own_order(
    input_schema: &'static Schema,
    collection: &str,
) -> Option<String> {
    let declared = element_constraint_set(&[input_schema], collection).or_else(|| {
        let nested = collection.rsplit('.').next()?;
        element_constraint_set(NESTED_SHAPES, nested)
    })?;
    let mut requirements: Vec<String> = declared
        .into_iter()
        .map(|kind| {
            let kind = match kind {
                ViolationKind::EnumValue(values) => {
                    ViolationKind::EnumValue(super::stated_enum_values(&values))
                }
                other => other,
            };
            requirement(kind)
        })
        .collect();
    // ...and, last, that the member be CARRIED at all, for a member the model never leaves
    // optional — one it declares `@required`, and one carrying a `@default`, which a request
    // cannot submit as null either. A member a request may omit altogether states the
    // element shape's constraints alone. Which of the two a member is read off this
    // service's own model ([`super::stated_element_sets`]), so the clause follows the
    // declaration rather than the operation a refusal came from. The sentence is the
    // framework's own for an absent member ([`requirement`]), so one wording serves both the
    // clause about a member nothing filled and this nested requirement.
    if super::stated_element_sets::requires_the_member(collection) {
        requirements.push(requirement(ViolationKind::MissingRequiredMember));
    }
    Some(element_set_requirement(&requirements))
}

/// The refusal this service states for one request, by the operation its routing key names, or
/// `None` for an operation this class cannot arise in and for a request that breaks no such
/// constraint.
pub fn stated(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<(Vec<Violation>, Refused)> {
    let key = protocol.routing_key(req).ok()?;
    collector(key)?(protocol, req)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an operation whose own input declares a list of scalars is collected, and one
    /// that declares none is not — so no request is read twice for a clause that cannot arise
    /// in it.
    #[test]
    fn the_operations_collected_are_the_ones_with_a_list_input() {
        for key in [
            "CreateConditionalForwarder",
            "CreateMicrosoftAD",
            "DescribeDirectories",
            "RemoveIpRoutes",
            "RemoveTagsFromResource",
        ] {
            assert!(
                collector(key).is_some(),
                "case {key:?}: expected the operation to be collected"
            );
        }
        for key in [
            "CreateAlias",
            "CreateSnapshot",
            "DeleteDirectory",
            "NotAnOperation",
        ] {
            assert!(
                collector(key).is_none(),
                "case {key:?}: expected the operation not to be collected"
            );
        }
    }
}
