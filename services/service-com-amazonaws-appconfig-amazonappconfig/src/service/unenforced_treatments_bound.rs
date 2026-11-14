// SPDX-License-Identifier: CC0-1.0

//! The one modelled constraint of this service that AppConfig does NOT apply on the wire:
//! the maximum number of entries an experiment definition's `Treatments` list may carry.
//!
//! `TreatmentInputList` is modelled `@length(min: 1, max: 5)` and both members that target
//! it — `CreateExperimentDefinitionRequest$Treatments` and
//! `UpdateExperimentDefinitionRequest$Treatments`
//! (`smithy_query(query="operation",operation="CreateExperimentDefinition,UpdateExperimentDefinition")`)
//! — are recorded SERVED with a longer list: a create carrying six treatments answers 201
//! with the definition `IDLE`, and an update carrying six answers 200 and the definition
//! then reports all six. So the maximum is a documentation bound on that list rather than a
//! server-side check, and a request that only breaks it must be served, not refused.
//!
//! Nothing else about the list is relaxed. Its `@length` MINIMUM is still enforced — an
//! empty `Treatments` is refused, as is every constraint the list's ELEMENTS declare
//! (`TreatmentInput$Weight`'s `@range`, `TreatmentInput$Description`'s length), since those
//! are checked at the element and not at the collection.
//!
//! What still bounds the list a definition holds: this service's own cap on how many
//! treatments one definition keeps (`crate::state::MAX_TREATMENTS_PER_EXPERIMENT_DEFINITION`,
//! checked by `crate::state::keyed_treatments` before a single treatment is built, so a
//! longer list allocates nothing and is refused), and the number of definitions one
//! application may hold (`crate::state::MAX_EXPERIMENT_DEFINITIONS_PER_APPLICATION`). The
//! member bound this module relaxes is a FIDELITY bound, not the resource bound: relaxing it
//! moves the clamp from the model's walk to those two caps rather than removing it.
//!
//! WHY a judgement in front of the walk, rather than a check in the handlers: the walk that
//! refuses the request is model-driven and runs inside the framework's own
//! deserialize -> validate -> handle -> serialize sequence, before any handler is reached.
//! A handler therefore cannot admit a list the walk rejects; the only layer that can is one
//! in front of the sequence, which is where [`super::InServiceDialect`] already words this
//! service's pre-handler refusals. A request whose ONLY violations are of this bound is
//! read, handled and serialized here — by the same three steps the framework's sequence
//! runs, through the same protocol and the same operation — and everything else is left to
//! that sequence untouched.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::{Violation, ViolationKind};
use smithy_server_runtime::{
    DeserializableStruct, ErasedOperation, Operation, OperationContext, ServerHttpProtocol, Service,
};

/// The member path a clause names an experiment definition's treatment list at, which is
/// also the prefix every one of its elements is reported under
/// (`treatments.<position>.member`).
///
/// Both operations that take the list bind it at the top of their input, so the path is the
/// member's own name as `aws_smithy_schema_server::constraints::path` spells one.
pub(super) const TREATMENTS: &str = "treatments";

/// Whether `violation` is the one requirement this service does not apply: the MAXIMUM
/// length of the treatment list itself.
///
/// Keyed on the member and the CLASS of violation alone — a maximum length broken by the
/// `treatments` COLLECTION — so it holds for every operation that takes such a list, and
/// for nothing else: the list's minimum keeps its refusal, and a violation one step deeper
/// (`treatments.<position>.member.<member>`) is a constraint of an ELEMENT, which AppConfig
/// does enforce.
pub(super) fn is_unenforced(violation: &Violation) -> bool {
    matches!(violation.kind, ViolationKind::MaxLength(_)) && violation.path == TREATMENTS
}

/// What this layer decided about one request, before anything is awaited.
///
/// Its own type rather than an `Option`, because there are THREE outcomes and only one of
/// them is an answer this layer built.
pub(super) enum Judgement {
    /// Refused here, in this service's own dialect. BOXED, because a response is far larger
    /// than the two decisions that carry nothing at all.
    Refused(Box<RtResponse>),
    /// Left to the framework's own deserialize -> validate -> handle -> serialize sequence,
    /// which is every ordinary request.
    TheFrameworksSequence,
    /// Served here, past the constraint walk: everything the walk found is the bound this
    /// service does not apply ([`is_unenforced`]).
    ServedPastTheWalk,
}

/// How `req` is served by `O`: refused in this service's words, handed to the framework's
/// sequence, or served past the walk.
///
/// The violations come from the framework's OWN deserializer and constraint walk, collected
/// by the one shared helper every service that words its own pre-handler refusal uses
/// (`service_common::validation::refusing_violations`), so this and the walk that follows it
/// cannot find a different set. A request the codec cannot read at all yields none and so
/// falls through to the framework's sequence, which answers it in its own envelope.
///
/// A refusal is worded over the ENFORCED violations ALONE, so a refusal never states a
/// requirement this service does not apply.
pub(super) fn judged<O: Operation>(svc: &dyn Service, req: &RtRequest) -> Judgement {
    // The protocol is read, used and dropped inside this one function, so nothing borrowed
    // from the service definition is held across the caller's await: `&dyn
    // ErasedServiceDefinition` is not `Sync`.
    let (unenforced, enforced): (Vec<Violation>, Vec<Violation>) =
        service_common::validation::refusing_violations::<O>(svc.definition().protocol(), req)
            .into_iter()
            .partition(is_unenforced);
    if !enforced.is_empty() {
        return match super::constraint_refusal(svc, req, enforced) {
            Some(response) => Judgement::Refused(Box::new(response)),
            None => Judgement::TheFrameworksSequence,
        };
    }
    // A walk that filled its collecting bound may have left a violation this service DOES
    // enforce out of the set, so a request at the bound is not one this layer can read as
    // carrying nothing enforceable: it goes to the framework's sequence, which refuses it.
    let at_the_collecting_bound = unenforced.len() >= service_common::validation::MAX_VIOLATIONS;
    if unenforced.is_empty() || at_the_collecting_bound {
        Judgement::TheFrameworksSequence
    } else {
        Judgement::ServedPastTheWalk
    }
}

/// Serve `req` HERE, past the constraint walk that would have refused it for the bound this
/// service does not apply.
///
/// The SAME sequence the framework's own runs — read the input through this service's
/// protocol, call the operation's `handle`, serialize its output or its modelled error — and
/// nothing else, so the behaviour answered is still the operation's own. A body no codec can
/// read names no values to judge and is left to the framework's own sequence.
pub(super) async fn served_past_the_walk<O: Operation + ErasedOperation>(
    operation: &O,
    svc: &dyn Service,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    // The protocol is read, used and dropped inside each statement below, for the reason
    // [`judged`] states.
    let read = {
        let protocol = svc.definition().protocol();
        read_input::<O>(protocol, req)
    };
    let Some(input) = read else {
        return operation.run(svc, req, ctx).await;
    };
    let answered = operation.handle(input, ctx).await;
    let protocol = svc.definition().protocol();
    match answered {
        Ok(output) => protocol
            .serialize_output(&output, O::OUTPUT_SCHEMA, O::NAME, O::SUCCESS_STATUS)
            .unwrap_or_else(|error| svc.definition().framework_error_response(error, req)),
        // The modelled error through the shared renderer, so a failure answered here and the
        // same failure answered from the framework's sequence are one response.
        Err(error) => service_common::event_stream::error_response(protocol, &error),
    }
}

/// `req` read into `O`'s input shape through the service's own protocol, or `None` when it
/// does not READ as that input at all.
///
/// Deliberately NOT validated: the whole point of this module is to serve a request the
/// constraint walk would have refused for a bound AppConfig does not apply.
fn read_input<O: Operation>(protocol: &dyn ServerHttpProtocol, req: &RtRequest) -> Option<O::Input> {
    let mut deserializer = protocol.deserialize_input(req, O::INPUT_SCHEMA).ok()?;
    let read = <O::Input as DeserializableStruct>::deserialize(&mut *deserializer);
    drop(deserializer);
    read.ok()
}

#[cfg(test)]
mod tests {
    use super::{is_unenforced, TREATMENTS};
    use aws_smithy_schema_server::{Violation, ViolationKind};

    /// One violation at `path` of class `kind`, as the walk builds one.
    fn violation(path: &str, kind: ViolationKind) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: Some("6 items".to_string()),
            sensitive: false,
        }
    }

    /// Only the treatment list's own MAXIMUM is unenforced: not its minimum, not a
    /// constraint of one of its elements, and not a maximum on any other member.
    #[test]
    fn the_treatment_list_maximum_is_the_only_unenforced_requirement() {
        let cases: Vec<(&str, Violation, bool)> = vec![
            (
                "the treatment list over its maximum",
                violation(TREATMENTS, ViolationKind::MaxLength(5)),
                true,
            ),
            (
                "the treatment list under its minimum",
                violation(TREATMENTS, ViolationKind::MinLength(1)),
                false,
            ),
            (
                "an element's weight under its range",
                violation(
                    "treatments.1.member.weight",
                    ViolationKind::MinValue("0".to_string()),
                ),
                false,
            ),
            (
                "an element's description over its maximum",
                violation(
                    "treatments.1.member.description",
                    ViolationKind::MaxLength(1024),
                ),
                false,
            ),
            (
                "another list over its maximum",
                violation("validators", ViolationKind::MaxLength(2)),
                false,
            ),
            (
                "a scalar member over its maximum",
                violation("name", ViolationKind::MaxLength(64)),
                false,
            ),
        ];

        for (case, violation, expected) in &cases {
            let actual = is_unenforced(violation);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected is_unenforced == {expected}, got {actual}"
            );
        }
    }
}
