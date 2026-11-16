// SPDX-License-Identifier: CC0-1.0

//! The three templates this service answers a `CacheParameterGroupNotFoundFault`
//! with, and which of them each operation takes.
//!
//! ElastiCache words that fault three ways. Each carries the
//! `CacheParameterGroupName` the request named and nothing this store holds; they
//! differ in where the name sits and in the separator between the type name and
//! the words:
//!
//! ```text
//! CacheParameterGroupnot found: <name>     <- no separator at all
//! CacheParameterGroup not found: <name>
//! CacheParameterGroup <name> not found.    <- the name first, with a full stop
//! ```
//!
//! WHICH of the three an operation answers with is that operation's own — they
//! are observable apart on the live service, so one service-wide template cannot
//! serve them all. [`joined_message`] is the first, which
//! `DeleteCacheParameterGroup`, `DescribeCacheParameters` and
//! `ResetCacheParameterGroup` answer; its missing space is the service's and not a
//! slip here, because the type name and `not found: ` arrive joined.
//! [`spaced_message`] is the second, which `ModifyCacheParameterGroup` answers and
//! which the operations resolving a `CacheParameterGroupName` as part of a larger
//! request take too. [`named_message`] is the third, which
//! `DescribeCacheParameterGroups` answers — the read a client uses to confirm a
//! group is gone.
//!
//! All three templates in one place, so no operation has to restate one and the
//! handlers answering the same template cannot drift apart about its text.

/// The first template: `CacheParameterGroupnot found: <name>`.
///
/// The one variable part is the `CacheParameterGroupName` the request named,
/// reported as the request spelled it, because the message is built from the
/// request and from nothing else.
pub fn joined_message(cache_parameter_group_name: &str) -> String {
    format!("CacheParameterGroupnot found: {cache_parameter_group_name}")
}

/// The second template: `CacheParameterGroup not found: <name>`, with the same
/// one variable part as [`joined_message`].
///
/// The text itself is the one `ModifyCacheParameterGroup` already answers with
/// ([`crate::state::parameter_writer_group_not_found_message`]), reached through
/// here so that every operation choosing between the templates names them
/// the same way and the spellings cannot drift apart.
pub fn spaced_message(cache_parameter_group_name: &str) -> String {
    crate::state::parameter_writer_group_not_found_message(cache_parameter_group_name)
}

/// The third template: `CacheParameterGroup <name> not found.` — the type name,
/// then the name the request supplied, then the words, with a trailing stop. The
/// same one variable part as [`joined_message`].
///
/// The text itself is [`crate::state::cache_parameter_group_not_found_message`],
/// the form this service words a resource's own not-found with, reached through
/// here for the reason [`spaced_message`] gives.
pub fn named_message(cache_parameter_group_name: &str) -> String {
    crate::state::cache_parameter_group_not_found_message(cache_parameter_group_name)
}
