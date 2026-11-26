// SPDX-License-Identifier: CC0-1.0

//! The ORDER a pool holds a collection of OAuth 2.0 scopes in: the scopes an app client
//! may authorize, and the custom scopes a resource server declares.
//!
//! `AllowedOAuthScopes` is a SET of scope text and nothing else — "the OAuth 2.0 scopes
//! that you want your app client to support", standard ones beside the custom ones a
//! resource server defines (`UserPoolClientType.AllowedOAuthScopes`) — and a pool holds
//! that set ordered by the text itself, not as the sequence a request wrote: the recorded
//! service answers every READ of an app client in the text's order, however the create or
//! the update that configured it sequenced the scopes.
//!
//! The CREATE's own response is the configuration it was handed, so there — and only there
//! — the member comes back as the submitted sequence: the recorded service answers one
//! create with a sequence its own later read of the same app client does not keep.
//!
//! A resource server's `Scopes` is a collection KEYED by the scope name, which the resource
//! server holds in the order the table keyed on those names is walked
//! ([`held_resource_server_scopes`]).
//!
//! Neither order follows a request: both carry nothing of an operation or a member, so one
//! collection of scopes reads the same way wherever it is carried, and configuring the same
//! scopes in another sequence moves nothing.

/// The scopes an app client may authorize, in the order the pool holds them: the scope
/// text's own order, because the collection is a set of that text.
#[must_use]
pub fn held_scopes(mut scopes: Vec<String>) -> Vec<String> {
    scopes.sort();
    scopes
}

/// The custom scopes a resource server holds, in the order it holds them: the order the
/// hash table keyed on each scope's own `ScopeName` is walked, rather than the sequence the
/// declaration wrote.
///
/// A resource server's custom scopes are a collection KEYED by the scope name — "a list of
/// custom scopes[.] Each scope is a key-value map with the keys `ScopeName` and
/// `ScopeDescription`", and one scope is addressed as `resourceServerIdentifier/ScopeName`
/// (`CreateResourceServer.Scopes`, `ResourceServerScopeType.ScopeName`) — so the resource
/// server holds them in a table keyed on that name and serves them in the order that table
/// is walked: ascending bucket of the name's own string hash at the table's width, two
/// names of one bucket in name order. That derivation is the shared one
/// ([`service_common::submitted_collections::hashed_entry_order`]), so a scope collection
/// is ordered the same way wherever one of these services holds it.
///
/// The order follows the scope NAMES and their count alone, so it carries nothing of a
/// request, an operation or a member: declaring one set of scopes in another sequence moves
/// nothing, and every read of the resource server — its own update's response, a describe
/// and a listing — renders the one held order.
#[must_use]
pub fn held_resource_server_scopes(
    declared: &[super::ResourceServerScopeType],
) -> Vec<super::ResourceServerScopeType> {
    let keyed: Vec<(&str, &str)> = declared
        .iter()
        .map(|scope| (scope.scope_name.as_str(), scope.scope_description.as_str()))
        .collect();
    service_common::submitted_collections::hashed_entry_order(&keyed)
        .into_iter()
        .map(|(scope_name, scope_description)| super::ResourceServerScopeType {
            scope_name: scope_name.to_string(),
            scope_description: scope_description.to_string(),
        })
        .collect()
}
