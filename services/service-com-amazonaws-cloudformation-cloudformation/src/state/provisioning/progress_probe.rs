// SPDX-License-Identifier: CC0-1.0

//! WHICH stacks a progress pass is for: every stack of the Region for a client's own call,
//! and the ONE stack being read for a read this service made while provisioning.
//!
//! ## The rule this module implements
//!
//! A reporting operation of CloudFormation materialises what the clock already owes before it
//! answers ([`super::advance_stabilisations`]): the services owning the resources still
//! coming up are asked what they report now, the resources a deployment withheld are created
//! once what they read has published, and the waits that have run out are failed. For a
//! client's call that is the whole Region's work — the call names one stack at most, but what
//! the clock owes it owes every stack.
//!
//! One resource type the pass asks about is owned by CloudFormation itself — the nested stack
//! a template declares ([`super::nested_stack`]) — and the question is put through
//! CloudFormation's own `DescribeStacks`. That read is served by a pass of its own, which is
//! what lets one client call settle a whole hierarchy: the child is advanced while the parent
//! is asking about it, so the answer the parent gets is the child's current one rather than
//! one call stale. What it must NOT do is advance the Region again, because the Region
//! includes the parent that is asking: each level would then ask about every level, once per
//! level, and one client call would be served by as many whole-Region passes as the
//! transport's recursion ceiling allows.
//!
//! So the pass such a read drives is SCOPED to the stack it is reading. A hierarchy is then
//! advanced from the bottom up in one call — each parent's question advances the child it
//! asks about, which advances the children IT asks about — and the work one call costs is one
//! pass per stack of the hierarchy rather than one per path through it.
//!
//! The scope travels on the CALL, as the nested-stack chain does, because that is where the
//! relation is: each read is a request of its own. Nothing a client can send widens what is
//! done for it — a scope only ever makes a pass do LESS, and it names the stack the request
//! is already about.

/// The header a read this service made while provisioning carries, whose value is the stack
/// that read is about.
pub(crate) const PROGRESS_SCOPE_HEADER: &str = "x-emulator-cloudformation-progress-scope";

/// The longest stack identifier the scope is sent for. Past it the read is served by the
/// Region's own pass: a constant rather than a length the request chooses, and far past both
/// a stack name's documented maximum and the ARN that carries one.
const MAX_SCOPE_LENGTH: usize = 512;

/// The header pair a provisioning read of `stack` is made with, or `None` for an identifier a
/// header value may not carry.
///
/// The value is the identifier the call ALREADY names the stack by, so it is sent only where
/// it is one a header value admits: visible ASCII, which every stack name and stack id this
/// service mints is ([`crate::state::STACK_IDENTIFIER_PATTERN`]). Anything else is left off
/// and the read is served by the Region's own pass — a WIDER pass, never a narrower one, so
/// nothing is left unadvanced on account of an identifier this could not carry.
pub(crate) fn probe_header(stack: &str) -> Option<(&'static str, String)> {
    let carriable = !stack.is_empty()
        && stack.len() <= MAX_SCOPE_LENGTH
        && stack.chars().all(|carried| carried.is_ascii_graphic());
    carriable.then(|| (PROGRESS_SCOPE_HEADER, stack.to_string()))
}

/// Which stacks one progress pass is for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    /// The stack a provisioning read named, as the request spelled it — a stack is
    /// addressable by name and by id, so both spellings are matched. `None` is a client's own
    /// call, which the whole Region's pass serves.
    only: Option<String>,
}

impl Scope {
    /// The scope the request being served asks for ([`PROGRESS_SCOPE_HEADER`]).
    ///
    /// Read only off a request that also carries the nested-stack CHAIN this service's own
    /// provisioning puts on every call it makes ([`super::nested_stack::CHAIN_HEADER`]): a
    /// scope NARROWS a pass, and the only reads one is for are the ones made from inside a
    /// pass. A client's own call carries no chain, so it is served by the Region's pass
    /// whatever headers it sends.
    pub(crate) fn of(ctx: &::smithy_server_runtime::OperationContext<'_>) -> Self {
        if !ctx
            .parts()
            .headers
            .contains_key(super::nested_stack::CHAIN_HEADER)
        {
            return Self::default();
        }
        Self {
            only: ctx
                .parts()
                .headers
                .get(PROGRESS_SCOPE_HEADER)
                .and_then(|named| named.to_str().ok())
                .filter(|named| !named.is_empty())
                .map(str::to_string),
        }
    }

    /// Whether this pass is for `stack`.
    pub(crate) fn covers(&self, stack: &crate::state::StoredStack) -> bool {
        match &self.only {
            Some(named) => stack.stack_id == *named || stack.stack_name == *named,
            None => true,
        }
    }
}
