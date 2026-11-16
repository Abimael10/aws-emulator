// SPDX-License-Identifier: CC0-1.0

//! How this service STATES a request that failed the rules its model declares.
//!
//! A modelled constraint is weighed at the request boundary, so a value outside one never
//! reaches a handler and the refusal is worded here instead ([`refusal`], reached from
//! [`crate::service::EmulatedService::override_refusal`]). The same wording is what a
//! HANDLER answers with when it weighs a leaf the boundary walk cannot reach ([`stated`],
//! called by the checks on [`crate::state`]), so which layer refused a value is not
//! something a caller can tell.
//!
//! WHAT is enforced is always the model's, and every sentence — the envelope, the member
//! paths, each requirement, the grouping of a collection element's constraint set and the
//! rendering of a `@pattern` — is the fleet's one home for it
//! ([`::service_common::validation_message`] and [`::service_common::validation`]), so a
//! clause this service states cannot drift from another service's for the same constraint.
//! Three things about such a report are this API's own, and they are all this module holds:
//!
//! * a `@pattern` is quoted as the expression BODY, without the anchors the model writes
//!   around it ([`::service_common::validation::unanchored`], re-exported here as
//!   [`unanchored`] for the handler-side clauses and the suites that pin them);
//! * the Amazon Connect instance id is never echoed: the member that carries one is named,
//!   its value is not ([`carries_connect_instance_id`]);
//! * a violation of a constraint on an ELEMENT of a list of scalars is reported against the
//!   LIST member, quoting the list and nesting the element shape's whole constraint set,
//!   which the shared renderer reads off this service's own input schemas
//!   ([`input_schemas`]);
//!
//! and one operation's refusal body is double-encoded: `PutOutboundRequestBatch` carries the
//! whole envelope nested in a `{ "message": … }` JSON document ([`in_json_document`]).

use ::aws_smithy_schema::Schema;
use ::service_common::validation::{list_of_element, rendered_list};
use ::smithy_server_runtime::{Refusal, Refused, Violation};

/// A `@pattern` as a refusal of this service quotes it: the expression body, without the
/// anchors the model writes around it.
///
/// The fleet's own rendering, re-exported rather than restated, so the clauses this
/// service's handlers compose and the ones its boundary states quote one expression one way.
pub use ::service_common::validation::unanchored;

/// The member a request carries its Amazon Connect instance id in, as every operation that
/// takes one spells it (`connectInstanceId`, from `smithy_query(query="operation",
/// operation="CreateCampaign")`).
const CONNECT_INSTANCE_ID_MEMBER: &str = "connectInstanceId";

/// The one OTHER place an Amazon Connect instance id arrives: `ListCampaigns`' instance
/// filter, whose `value` targets the same `InstanceId` shape
/// (`smithy_query(query="shape", shape="InstanceIdFilter")`).
const INSTANCE_ID_FILTER_VALUE: &str = "filters.instanceIdFilter.value";

/// Whether the member at `path` carries an Amazon Connect instance id, whose value this
/// service does not echo back.
fn carries_connect_instance_id(path: &str) -> bool {
    path == INSTANCE_ID_FILTER_VALUE || path.rsplit('.').next() == Some(CONNECT_INSTANCE_ID_MEMBER)
}

/// `PutOutboundRequestBatch`' own list member, the one member no other operation of this
/// model declares (`smithy_query(query="operation", operation="PutOutboundRequestBatch")`).
///
/// It is how a refusal of THAT operation is told from every other one's: the boundary seam
/// is handed the violations and not the operation they came from, and this operation is the
/// one whose refusal body AWS double-encodes. Keying the choice on the operation itself
/// would take the refusal seam in the agent-immutable `smithy-server-runtime` carrying that
/// identity, which a service crate cannot change; until it does, the member this model
/// declares exactly once is what names the operation.
const OUTBOUND_REQUESTS_MEMBER: &str = "outboundRequests";

/// Every INPUT shape of this service, as the generated schema statics.
///
/// All of them, not the ones one report happened to cite: an element constraint set is a
/// property of an input shape, so leaving an operation out would silently word its
/// collection clauses with only the constraints that failed. Regenerate with
/// `grep -h 'INPUT_SCHEMA: &' src/ops/*.rs`, which names exactly these statics.
fn input_schemas() -> Vec<&'static Schema> {
    use crate::ops::types as t;
    vec![
        t::CREATE_CAMPAIGN_INPUT_SCHEMA,
        t::DELETE_CAMPAIGN_CHANNEL_SUBTYPE_CONFIG_INPUT_SCHEMA,
        t::DELETE_CAMPAIGN_COMMUNICATION_LIMITS_INPUT_SCHEMA,
        t::DELETE_CAMPAIGN_COMMUNICATION_TIME_INPUT_SCHEMA,
        t::DELETE_CAMPAIGN_ENTRY_LIMITS_INPUT_SCHEMA,
        t::DELETE_CAMPAIGN_INPUT_SCHEMA,
        t::DELETE_CONNECT_INSTANCE_CONFIG_INPUT_SCHEMA,
        t::DELETE_CONNECT_INSTANCE_INTEGRATION_INPUT_SCHEMA,
        t::DELETE_INSTANCE_ONBOARDING_JOB_INPUT_SCHEMA,
        t::DESCRIBE_CAMPAIGN_INPUT_SCHEMA,
        t::GET_CAMPAIGN_STATE_BATCH_INPUT_SCHEMA,
        t::GET_CAMPAIGN_STATE_INPUT_SCHEMA,
        t::GET_CONNECT_INSTANCE_CONFIG_INPUT_SCHEMA,
        t::GET_INSTANCE_COMMUNICATION_LIMITS_INPUT_SCHEMA,
        t::GET_INSTANCE_ONBOARDING_JOB_STATUS_INPUT_SCHEMA,
        t::LIST_CAMPAIGNS_INPUT_SCHEMA,
        t::LIST_CONNECT_INSTANCE_INTEGRATIONS_INPUT_SCHEMA,
        t::LIST_TAGS_FOR_RESOURCE_INPUT_SCHEMA,
        t::PAUSE_CAMPAIGN_INPUT_SCHEMA,
        t::PUT_CONNECT_INSTANCE_INTEGRATION_INPUT_SCHEMA,
        t::PUT_INSTANCE_COMMUNICATION_LIMITS_INPUT_SCHEMA,
        t::PUT_OUTBOUND_REQUEST_BATCH_INPUT_SCHEMA,
        t::PUT_PROFILE_OUTBOUND_REQUEST_BATCH_INPUT_SCHEMA,
        t::RESUME_CAMPAIGN_INPUT_SCHEMA,
        t::START_CAMPAIGN_INPUT_SCHEMA,
        t::START_INSTANCE_ONBOARDING_JOB_INPUT_SCHEMA,
        t::STOP_CAMPAIGN_INPUT_SCHEMA,
        t::TAG_RESOURCE_INPUT_SCHEMA,
        t::UNTAG_RESOURCE_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_CHANNEL_SUBTYPE_CONFIG_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_COMMUNICATION_LIMITS_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_COMMUNICATION_TIME_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_ENTRY_LIMITS_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_FLOW_ASSOCIATION_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_NAME_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_SCHEDULE_INPUT_SCHEMA,
        t::UPDATE_CAMPAIGN_SOURCE_INPUT_SCHEMA,
    ]
}

/// The list a clause stated at the collection member `collection` quotes: the elements the
/// refusal NAMES, in request order, rendered the way AWS echoes a list-valued member
/// ([`rendered_list`]).
///
/// The elements come from the violations because this seam is handed those alone: the walk
/// reports one violation per offending element and carries nothing of the elements it
/// accepted, and `Service::override_refusal` is not given the request to read them from. One
/// element that broke two of its shape's constraints is named once, and the positions the
/// walk spells are what puts them in order.
fn quoted_collection(collection: &str, violations: &[Violation]) -> Option<String> {
    let mut elements: Vec<(usize, &str)> = violations
        .iter()
        .filter(|violation| list_of_element(&violation.path) == Some(collection))
        .filter_map(|violation| {
            let position = violation
                .path
                .trim_end_matches(".member")
                .rsplit('.')
                .next()?
                .parse::<usize>()
                .ok()?;
            Some((position, violation.value.as_deref().unwrap_or("null")))
        })
        .collect();
    if elements.is_empty() {
        return None;
    }
    elements.sort_by_key(|(position, _)| *position);
    elements.dedup_by_key(|(position, _)| *position);
    let rendered: Vec<&str> = elements.into_iter().map(|(_, value)| value).collect();
    Some(rendered_list(&rendered))
}

/// Whether `violations` belong to the outbound-request batch, the one operation whose
/// refusal body is double-encoded.
fn belongs_to_outbound_batch(violations: &[Violation]) -> bool {
    violations
        .iter()
        .any(|violation| violation.path.split('.').next() == Some(OUTBOUND_REQUESTS_MEMBER))
}

/// `message` as the body this service double-encodes a refusal in: the envelope nested in a
/// one-member JSON document, with the padding AWS writes around it.
///
/// `PutOutboundRequestBatch` is the one operation whose validation failures carry this
/// envelope, so a client reading its refusal decodes `Error.Message` to reach the clause.
///
/// The nesting is the INTERPOLATION the recorded refusal of this operation shows, not a
/// second serialization of the message: the recorded document carries the `@pattern`'s own
/// backslash (`[a-zA-Z0-9_\-.]*`) as it stands inside the quoted member, which escaping the
/// nested text would turn into a sequence AWS does not state. The response this travels in
/// is encoded by the protocol's own serializer, which escapes the whole message, so the
/// document reaches a client exactly as AWS sends one.
pub fn in_json_document(message: &str) -> String {
    format!("{{ \"message\": \"{message}\" }}")
}

/// How this service states `refusal`, or `None` to keep the framework's own wording.
///
/// Only a refusal of the request's VALUES is reworded; an authentication refusal says
/// nothing about a member and is left to the framework, which the shared renderer decides.
pub fn refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let stated =
        ::service_common::validation_message::aws_validation_refusal_over_elements_quoting_withholding(
            refusal,
            &input_schemas(),
            unanchored,
            |collection| quoted_collection(collection, violations),
            carries_connect_instance_id,
        )?;
    if belongs_to_outbound_batch(violations) {
        return Some(Refused {
            message: in_json_document(&stated.message),
            ..stated
        });
    }
    Some(stated)
}

/// `violations` as the message a `ValidationException` carries, or `None` when there is
/// nothing to state — the same wording [`refusal`] gives the boundary, for a HANDLER that
/// weighs a leaf the boundary walk cannot reach (a map's key, a value behind a union
/// member).
pub fn stated(violations: &[Violation]) -> Option<String> {
    refusal(&Refusal::Invalid(violations.to_vec())).map(|refused| refused.message)
}

#[cfg(test)]
mod tests {
    use ::smithy_server_runtime::ViolationKind;

    use super::*;

    /// A violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: a member carrying an Amazon Connect instance id is named without its value;
    /// every other member's value is echoed. The rows span the two members of this service's
    /// inputs that carry one and the members that do not.
    #[test]
    fn the_connect_instance_id_is_never_echoed() {
        let rows: [(&str, bool); 5] = [
            ("connectInstanceId", true),
            ("filters.instanceIdFilter.value", true),
            ("name", false),
            ("id", false),
            ("outboundRequests.1.member.clientToken", false),
        ];
        for (path, elided) in rows {
            let stated = stated(&[violation(
                path,
                ViolationKind::Pattern("^[a-z]+$".to_string()),
                Some("not a valid value!"),
            )])
            .expect("a refusal is stated");
            assert_eq!(
                !stated.contains("not a valid value!"),
                elided,
                "case {path:?}: expected the value {}, got {stated:?}",
                if elided { "elided" } else { "echoed" }
            );
            assert!(
                stated.contains(&format!("at '{path}'")),
                "case {path:?}: expected the clause to name the member, got {stated:?}"
            );
        }
    }

    /// RULE: a constraint violation on an element of a list of scalars is stated against the
    /// LIST member, quoting the list and nesting the element shape's whole constraint set as
    /// the model declares it. The rows span the number of elements that failed and the order
    /// the walk reported them in.
    #[test]
    fn a_scalar_lists_elements_are_stated_against_the_list() {
        let one_bad_id = stated(&[violation(
            "campaignIds.1.member",
            ViolationKind::Pattern(crate::state::CAMPAIGN_ID_PATTERN.to_string()),
            Some("not a valid id!"),
        )])
        .expect("a refusal is stated");
        assert_eq!(
            one_bad_id,
            "1 validation error detected: Value '[not a valid id!]' at 'campaignIds' failed to \
             satisfy constraint: Member must satisfy constraint: [Member must have length less \
             than or equal to 256, Member must have length greater than or equal to 1, Member \
             must satisfy regular expression pattern: [-:/a-zA-Z0-9]+]",
            "case \"one bad campaign id\": got {one_bad_id:?}"
        );

        // Two elements failing is still ONE clause about the list, naming both in request
        // order whichever order the walk reported them in.
        let two_bad_ids = stated(&[
            violation("campaignIds.2.member", ViolationKind::MinLength(1), Some("")),
            violation(
                "campaignIds.1.member",
                ViolationKind::Pattern(crate::state::CAMPAIGN_ID_PATTERN.to_string()),
                Some("bad id!"),
            ),
        ])
        .expect("a refusal is stated");
        assert!(
            two_bad_ids.starts_with("1 validation error detected: Value '[bad id!, ]' at 'campaignIds'"),
            "case \"two bad campaign ids\": got {two_bad_ids:?}"
        );

        // A list no input shape of this service declares keeps the framework's per-element
        // clause: the set a grouped clause nests is read off the model, so there is none to
        // state.
        let unknown_list = stated(&[violation(
            "someOtherList.1.member",
            ViolationKind::MinLength(1),
            Some(""),
        )])
        .expect("a refusal is stated");
        assert!(
            unknown_list.contains("at 'someOtherList.1.member'"),
            "case \"a list no input shape declares\": got {unknown_list:?}"
        );
    }

    /// RULE: the outbound-request batch's refusal is nested in a JSON document; no other
    /// operation's is.
    #[test]
    fn only_the_outbound_batch_refusal_is_double_encoded() {
        let batch = stated(&[violation(
            "outboundRequests.1.member.clientToken",
            ViolationKind::Pattern(r"^[a-zA-Z0-9_\-.]*$".to_string()),
            Some("bad token!"),
        )])
        .expect("a refusal is stated");
        assert_eq!(
            batch,
            "{ \"message\": \"1 validation error detected: Value 'bad token!' at \
             'outboundRequests.1.member.clientToken' failed to satisfy constraint: Member must \
             satisfy regular expression pattern: [a-zA-Z0-9_\\-.]*\" }",
            "case \"a bad client token\": got {batch:?}"
        );

        let profile_batch = stated(&[violation(
            "profileOutboundRequests.1.member.clientToken",
            ViolationKind::Pattern(r"^[a-zA-Z0-9_\-.]*$".to_string()),
            Some("bad token!"),
        )])
        .expect("a refusal is stated");
        assert!(
            !profile_batch.starts_with('{'),
            "case \"the profile batch\": expected the bare envelope, got {profile_batch:?}"
        );
    }

    /// RULE: nothing to state is no refusal, and an authentication refusal keeps the
    /// framework's wording.
    #[test]
    fn only_a_value_refusal_is_reworded() {
        assert_eq!(
            stated(&[]),
            None,
            "case \"no violations\": expected no message"
        );
        assert_eq!(
            refusal(&Refusal::Unauthenticated),
            None,
            "case \"an unauthenticated caller\": expected the framework's wording"
        );
        let chosen = refusal(&Refusal::Invalid(vec![violation(
            "name",
            ViolationKind::MissingRequiredMember,
            None,
        )]));
        assert_eq!(
            chosen.map(|refused| (refused.code, refused.message)),
            Some((
                "ValidationException".to_string(),
                "1 validation error detected: Value null at 'name' failed to satisfy constraint: \
                 Member must not be null"
                    .to_string(),
            )),
            "case \"an absent required member\": expected the framework's clause under this \
             service's code"
        );
    }
}
