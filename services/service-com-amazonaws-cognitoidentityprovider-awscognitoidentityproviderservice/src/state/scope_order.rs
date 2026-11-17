// SPDX-License-Identifier: CC0-1.0

//! The ORDER a pool holds the OAuth 2.0 scopes an app client may authorize in.
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
//! The order follows the scope text alone, so it carries nothing of a request, an
//! operation or a member: one set of scopes reads the same way wherever it is carried, and
//! configuring the same set in another sequence moves nothing.

/// The scopes an app client may authorize, in the order the pool holds them: the scope
/// text's own order, because the collection is a set of that text.
#[must_use]
pub fn held_scopes(mut scopes: Vec<String>) -> Vec<String> {
    scopes.sort();
    scopes
}
