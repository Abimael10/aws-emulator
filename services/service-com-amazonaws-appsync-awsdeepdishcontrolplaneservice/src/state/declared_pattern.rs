// SPDX-License-Identifier: CC0-1.0

//! How a modelled `@pattern` this crate checks OUTSIDE the request boundary is evaluated.
//!
//! A handler checks a `@pattern` itself where the refusal's wording is the operation's own
//! rather than the boundary's (`skip_validation` on the member, and the handler's own
//! sentence: [`crate::state::api_name_refusal`],
//! [`crate::state::resource_name_refusal`]). Every such check evaluates the expression the
//! same way, so the reading lives here once:
//!
//! * the expression is a CONSTANT of the model, so it is compiled once for the process
//!   ([`CompiledPattern`]) and never per request;
//! * the engine is the one the request boundary's own constraint walk evaluates a
//!   `@pattern` with, so a handler's check and the boundary's cannot read one expression
//!   two ways;
//! * an expression that does not compile matches, exactly as the framework's own walk
//!   reads one: no value is refused because of an unreadable model pattern.
//!
//! A Smithy `@pattern` is a FIND rather than a total match, which is what
//! [`::regex::Regex::is_match`] is; an expression that anchors both ends itself then
//! describes the whole value.

/// A modelled `@pattern` compiled once for the process, or `None` for one that does not
/// compile.
pub(crate) type CompiledPattern = ::std::sync::LazyLock<Option<::regex::Regex>>;

/// Whether `candidate` satisfies `expression`.
pub(crate) fn matches_declared_pattern(expression: &CompiledPattern, candidate: &str) -> bool {
    expression
        .as_ref()
        .is_none_or(|expression| expression.is_match(candidate))
}
