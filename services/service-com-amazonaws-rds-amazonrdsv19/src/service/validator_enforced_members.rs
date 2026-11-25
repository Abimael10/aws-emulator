// SPDX-License-Identifier: CC0-1.0

//! The absent parameters this front door's own REQUEST VALIDATOR reports, as against the
//! ones the operation behind it asks for.
//!
//! Every member this model marks `@required` also carries `@clientOptional`, so presence is
//! not enforced by the client and a request missing one reaches the service. Which layer
//! there reports it decides how it is worded, and the two statements are different:
//!
//! * the request validator in front of the operation reports it as the constraint the
//!   request broke, in the framework's own clause (`1 validation error detected: Value null
//!   at '<member>' failed to satisfy constraint: Member must not be null`), which is the
//!   text that class of refusal carries wherever it is raised;
//! * the operation itself reports it by naming the parameter the caller has to supply
//!   ([`super::missing_parameter`]), with nothing else of the request in the sentence.
//!
//! Recorded behaviour decides which operations are validated in front: this module holds
//! that list ([`VALIDATOR_ENFORCED_INPUTS`]) and answers a request missing one of those
//! members with the first statement, while every other operation keeps the second. It sits
//! in front of the framework's read of the request for the same reason
//! [`super::unresolved_reference`] does: the framework finds an absent member while reading
//! the input, so the handler never runs and the refusal is worded by the one seam the
//! service has for a refusal raised there — which cannot tell the operations apart, because
//! a refusal carries the member it is about and not the action that carried it.
//!
//! Only the ABSENCE of a member is decided here. A member the request DID carry is held to
//! its own domain by the operation that reads it, whatever this list says.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::{constraints::Violation, ProtocolError, ViolationKind};
use smithy_server_runtime::{Refused, Service};

/// The inputs whose absent required members the request validator in front of the operation
/// reports, each as the `Action` and the members that input marks `@required`
/// (`smithy_query(query="shape",shape="CreateDBClusterMessage",
/// model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`
/// reports `required: true` for `DBClusterIdentifier` and `Engine`, and for no other member
/// of it).
///
/// Recorded behaviour: a cluster create missing one of the two members it cannot be served
/// without is answered with the validator's clause, while the reports and modifications of
/// this same front door are answered by naming the parameter
/// ([`super::missing_parameter`]). The list is per INPUT rather than per member, because the
/// layer that enforces presence enforces it for the whole input it validates.
const VALIDATOR_ENFORCED_INPUTS: [(&str, &[&str]); 1] =
    [("CreateDBCluster", &["DBClusterIdentifier", "Engine"])];

/// The member a refusal of this front door names: the model's member name with its first
/// character lowered, the spelling the framework's validating walk records a member position
/// in (`Engine` is named `engine`).
fn member_path(member: &str) -> String {
    let mut characters = member.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The members [`VALIDATOR_ENFORCED_INPUTS`] names for this request's `Action` that the
/// request does not carry at all, in the order the input declares them.
///
/// Empty for every other request: another action, or one carrying each of those members. A
/// member the request carried — even empty — is a value, which this module says nothing
/// about.
fn absent_members(req: &RtRequest) -> Vec<String> {
    let Some(body) = req.body().bytes() else {
        return Vec::new();
    };
    let Some(action) = aws_smithy_schema_server::extract_action(body) else {
        return Vec::new();
    };
    let Some((_, required)) = VALIDATOR_ENFORCED_INPUTS
        .iter()
        .find(|(validated, _)| *validated == action)
    else {
        return Vec::new();
    };
    let pairs = aws_smithy_schema_server::parse_form_urlencoded(body);
    required
        .iter()
        .filter(|member| !pairs.iter().any(|(name, _)| name == *member))
        .map(|member| member_path(member))
        .collect()
}

/// The front door's validation refusal for such a request, or `None` when there is nothing
/// to report: an action this module does not claim, or a request carrying every member it
/// names.
///
/// The CODE is this service's for every parameter the request did not supply acceptably, as
/// it is for the refusals the framework raises ([`super::invalid_parameter_value`]); the
/// SENTENCE is the framework's own clause for an absent member, built by the one writer of
/// it, so this refusal and the identical one raised while reading any other request are the
/// same text. The status and the envelope stay with the protocol, which renders them from
/// the same refusal the framework would have rendered.
pub fn refuse_absent_member(svc: &dyn Service, req: &RtRequest) -> Option<RtResponse> {
    let absent = absent_members(req);
    let violations: Vec<Violation> = absent
        .iter()
        .map(|path| Violation {
            path: path.clone(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        })
        .collect();
    let message = aws_smithy_schema_server::envelope(&violations)?;
    Some(svc.definition().refusal_response(
        ProtocolError::Invalid(violations),
        Some(Refused {
            code: super::INVALID_PARAMETER_VALUE.to_string(),
            message,
        }),
        req,
    ))
}
