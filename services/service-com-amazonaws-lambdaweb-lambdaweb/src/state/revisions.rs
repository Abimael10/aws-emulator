// SPDX-License-Identifier: CC0-1.0

//! The revision slice of this service's state: how a request names a revision, and the
//! rule a revision's DELETE follows.
//!
//! A revision belongs to exactly one web function, so it lives inside that function's
//! record (`WebFunction::revisions`) and the function's own delete takes its revisions
//! with it. What is here is what the revision's delete needs and no reader of a revision
//! holds a second copy of: resolving the identifier a request named it by, the one
//! sentence an absent revision is reported with, and the removal itself — which also
//! moves the function's `latestRevisionId` off a revision that is gone, so every reader
//! of "the newest revision" reports one the store still holds.

use std::time::SystemTime;

use super::{ComAmazonawsLambdawebLambdawebState, Revision, WebFunction};

/// The `revisionId` a request names, resolved to the key the store holds.
///
/// The member's documentation says the caller "can specify either of the following: The
/// revision identifier. The revision ARN." (source: `smithy_query(query="doc",
/// shape="DeleteWebFunctionRevisionRequest", member="revisionId")`), so a value in the
/// ARN form resolves to the id in its `revision/<revisionId>` resource segment — the
/// layout [`super::revision_arn`] mints. Anything else is already an id.
pub fn resolve_revision_id(value: &str) -> &str {
    if !value.starts_with("arn:") {
        return value;
    }
    value
        .split_once("/revision/")
        .map_or(value, |(_, revision_id)| revision_id)
}

/// The `ResourceNotFoundException` message for a revision a function does not carry,
/// naming it by `identifier` — the value the request addressed it by, verbatim — and
/// naming the function it was looked for under.
///
/// The model fixes the status (404), the code and `message` being `@required`, but not
/// the text, so the sentence is the service's recorded behaviour. One function serves
/// every branch that reports an absent revision, so the wording cannot drift between the
/// readers, the delete and the endpoint weight rule.
pub fn revision_not_found_message(identifier: &str, function_name: &str) -> String {
    format!("The revision {identifier} of web function {function_name} doesn't exist.")
}

/// The endpoint of `function` that serves `revision_id`, or `None` when none does.
///
/// An endpoint's `revisionWeights` is what "maps a revision to a weight", so a revision
/// named by any endpoint's weights is one taking that endpoint's traffic.
pub fn endpoint_serving_revision<'store>(
    function: &'store WebFunction,
    revision_id: &str,
) -> Option<&'store str> {
    function
        .endpoints
        .values()
        .find(|endpoint| {
            endpoint
                .revision_weights
                .iter()
                .any(|weight| weight.revision_id == revision_id)
        })
        .map(|endpoint| endpoint.endpoint_name.as_str())
}

/// The `ConflictException` message for a revision whose delete is refused because it is
/// still taking an endpoint's traffic.
///
/// `DeleteWebFunctionRevision` documents the rule itself — "You cannot delete a revision
/// that is currently serving traffic on an endpoint." — and the modelled
/// `ConflictException` names this case among the conflicts it carries ("you tried to
/// delete a revision that is serving traffic on an endpoint"), so the refusal states
/// which revision cannot be deleted, which endpoint is serving it, and what the caller
/// has to change to proceed.
pub fn revision_serving_traffic_message(revision_id: &str, endpoint_name: &str) -> String {
    format!(
        "The revision {revision_id} is serving traffic on endpoint {endpoint_name} and cannot be \
         deleted. Shift the endpoint's traffic to another revision and try again."
    )
}

impl ComAmazonawsLambdawebLambdawebState {
    /// Remove the revision `revision_id` of `function_name`, returning the record that
    /// was removed, or `None` when the function or the revision is not stored.
    ///
    /// Removing a revision mutates the function, so its `updated_at` moves to `now`, and
    /// the function's `latestRevisionId` is re-derived from the revisions that are LEFT —
    /// the newest of them by creation, or none at all once the last one is gone — so the
    /// "newest revision" every later reader and the auto-deployment rule act on is a
    /// revision the store still holds.
    pub fn remove_revision(
        &mut self,
        function_name: &str,
        revision_id: &str,
        now: SystemTime,
    ) -> Option<Revision> {
        let function = self.functions.get_mut(function_name)?;
        let removed = function.revisions.remove(revision_id)?;
        function.latest_revision_id = function
            .revisions
            .values()
            .max_by_key(|revision| (revision.created_at, revision.revision_id.clone()))
            .map(|revision| revision.revision_id.clone());
        function.updated_at = now;
        Some(removed)
    }
}
