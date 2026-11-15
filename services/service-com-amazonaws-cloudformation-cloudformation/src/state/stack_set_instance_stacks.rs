// SPDX-License-Identifier: CC0-1.0

//! The one EXPLANATION a stack instance reports for the stack it deploys having FAILED.
//!
//! ## The rule this module implements
//!
//! A stack instance has ONE field for it — `StatusReason`, "the explanation for the specific
//! status code assigned to this stack instance" — and what failed its deployment is a
//! RESOURCE of the stack it deploys, which no read of the stack set can reach. So the
//! explanation names that resource, as the stack's own event for it reports it: the logical id
//! it was declared under (`StackEvent$LogicalResourceId`), its type
//! (`StackEvent$ResourceType`) and the "success/failure message associated with the resource"
//! (`StackEvent$ResourceStatusReason`), each labelled with the member it came from. A stack
//! whose failure no resource of it carries — one CloudFormation refused before any owning
//! service was asked anything — is explained by the stack's own `StackStatusReason` instead,
//! which is then the whole of what failed it.
//!
//! That wording is the INSTANCE's, not the stack's: the stack-level
//! `StackStatusReason` of a resource failure is the summary of WHICH resources failed
//! ([`super::failure_summary`]), which says nothing about why, and a client reading a stack
//! instance cannot reach the stack's events to find out. So this module is the one place it is
//! worded, and every path that fails an instance states it from here, whoever observed the
//! failure: the handler whose own deployment walk found it
//! ([`crate::ops::create_stack_instances`], [`super::stack_set_rollout`]) and the settle that
//! finds what the clock made due later ([`super::stack_set_instance_outcomes`]) report one
//! explanation for one failure.
//!
//! ## Bounds
//!
//! One pass over the stack's own event log, which is bounded where it is written
//! ([`super::MAX_EVENTS_PER_STACK`]), and one string per failed instance. Nothing is stored
//! here.

use super::StoredStack;

/// How the failure of `stack` is explained to the stack instance that deploys it, or `None`
/// for a stack whose failure nothing was recorded about.
///
/// The RESOURCE that failed is what the explanation names ([`failed_resource_reason`]), read
/// off the event the stack recorded for it; a stack with no such event is explained by its own
/// `StackStatusReason`.
#[must_use]
pub fn failure_reason(stack: &StoredStack) -> Option<String> {
    failed_resource(stack).or_else(|| stack.status_reason.clone())
}

/// The first resource of `stack` the operation that last acted on it could not carry through,
/// worded for a stack instance.
fn failed_resource(stack: &StoredStack) -> Option<String> {
    let opened_at = stack.last_operation().map(|operation| operation.started_at);
    stack
        .events
        .iter()
        // The operation NOW ending is the one whose failure the instance reports, so an event
        // of an earlier attempt on the same stack is not it.
        .filter(|event| opened_at.is_none_or(|at| event.timestamp >= at))
        .find(|event| {
            // The stack's OWN event reports the stack as its resource
            // ([`super::StoredStack::record_event`]): what it carries is the summary of which
            // resources failed, not the diagnosis of one of them.
            event.resource_type != super::STACK_RESOURCE_TYPE
                && event.resource_status.ends_with(super::FAILED_SUFFIX)
        })
        .map(|event| {
            failed_resource_reason(
                &event.logical_resource_id,
                &event.resource_type,
                event.resource_status_reason.as_deref(),
            )
        })
}

/// How the failure of ONE resource of an instance's stack is stated to the stack instance.
///
/// The three members of the stack's own event for that resource, each labelled with the
/// member it came from: `StackEvent$LogicalResourceId`, `StackEvent$ResourceType` and
/// `StackEvent$ResourceStatusReason`, in that order. A resource whose event carries no
/// message states the first two alone, since there is no third to state.
#[must_use]
pub fn failed_resource_reason(
    logical_id: &str,
    resource_type: &str,
    reason: Option<&str>,
) -> String {
    let mut stated = format!("ResourceLogicalId:{logical_id}, ResourceType:{resource_type}");
    if let Some(reason) = reason {
        stated.push_str(", ResourceStatusReason:");
        stated.push_str(reason);
    }
    stated
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource's failure is stated as the three members of its own event, labelled, and
    /// drops the message for an event that carries none.
    #[test]
    fn a_resource_failure_states_its_event_members() {
        for (case, logical_id, resource_type, reason, expected) in [
            (
                "a resource whose event carries a message",
                "Wait",
                "AWS::CloudFormation::WaitCondition",
                Some("WaitCondition timed out. Received 0 conditions when expecting 1"),
                "ResourceLogicalId:Wait, ResourceType:AWS::CloudFormation::WaitCondition, \
                 ResourceStatusReason:WaitCondition timed out. Received 0 conditions when \
                 expecting 1",
            ),
            (
                "a resource whose event carries none",
                "Param",
                "AWS::SSM::Parameter",
                None,
                "ResourceLogicalId:Param, ResourceType:AWS::SSM::Parameter",
            ),
        ] {
            let actual = failed_resource_reason(logical_id, resource_type, reason);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
