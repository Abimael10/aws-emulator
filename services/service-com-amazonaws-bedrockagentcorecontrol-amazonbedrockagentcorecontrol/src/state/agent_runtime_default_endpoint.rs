// SPDX-License-Identifier: CC0-1.0

//! The endpoint an agent runtime is CREATED with, and what it means for the runtime's
//! own delete.
//!
//! An agent runtime is invocable the moment it exists, and what an invocation is
//! addressed to is an endpoint, so the create establishes one itself: the endpoint named
//! [`DEFAULT_RUNTIME_ENDPOINT`] is the service's, not the caller's. Two consequences the
//! handlers read off this module, so no one of them can word them differently:
//!
//! * it TRACKS the runtime instead of pinning a version. A caller-created endpoint pins
//!   the version it named and stays there while the runtime publishes more
//!   ([`AgentRuntimeEndpointRecord`]); the service-managed one moves onto every newly
//!   published version by itself ([`AgentRuntimeRecord::track_published_version`]), which
//!   is what makes it the address of "whatever this runtime runs now";
//! * it is not the caller's to tear down, so it does NOT stand in the way of the
//!   runtime's own delete: a delete that would remove a runtime still holding a
//!   CALLER-created endpoint is refused ([`AGENT_RUNTIME_HAS_ENDPOINTS`]), and the
//!   managed endpoint goes when the runtime it belongs to goes.
//!
//! Its clock-owed transition is the runtime version's own: the endpoint is established
//! BY the create (or BY the publish), so it is ready exactly when the version it fronts
//! is, rather than one further span later as a caller's own create is
//! ([`AGENT_RUNTIME_ENDPOINT_SETTLE_DELAY`]).

use super::*;

/// Whether `endpoint_name` names the endpoint the service manages for a runtime rather
/// than one a caller created.
///
/// The name is what an endpoint is addressed by, and the managed one always carries
/// [`DEFAULT_RUNTIME_ENDPOINT`], so the question is answered by the name alone — the same
/// name the runtime's vended log group is derived under.
pub fn is_service_managed_endpoint(endpoint_name: &str) -> bool {
    endpoint_name == DEFAULT_RUNTIME_ENDPOINT
}

/// How this service words the refusal of a delete of an agent runtime that still holds
/// endpoints a caller created.
///
/// An endpoint is a stable address callers invoke, so it is torn down deliberately rather
/// than destroyed underneath them: the runtime is left exactly as it was and the caller
/// deletes its endpoints first. Entirely fixed text — it names neither the runtime nor
/// any endpoint, because the resources the caller has to remove are its own request's.
pub const AGENT_RUNTIME_HAS_ENDPOINTS: &str =
    "This agent has endpoints that must be deleted before the agent can be removed.";

/// The service-managed endpoint record a runtime is created with: the reserved name, the
/// ARN that name gives it under its runtime, and the newest published `version` as the
/// version it tracks.
///
/// `endpoint_id` is minted by the store exactly as a caller-created endpoint's is, so one
/// endpoint is never two kinds of resource.
pub fn managed_default_endpoint(
    region: &str,
    agent_runtime_id: &str,
    endpoint_id: String,
    version: &AgentRuntimeVersionRecord,
    now: SystemTime,
) -> AgentRuntimeEndpointRecord {
    AgentRuntimeEndpointRecord {
        endpoint_id,
        endpoint_name: DEFAULT_RUNTIME_ENDPOINT.to_string(),
        endpoint_arn: runtime_endpoint_arn(region, agent_runtime_id, DEFAULT_RUNTIME_ENDPOINT),
        // The caller named no endpoint, so it supplied no description either.
        description: None,
        target_version: version.version,
        // Nothing is served behind the name until the version it fronts is ready.
        live_version: None,
        created_at: now,
        last_updated_at: now,
        transient_status: STATUS_CREATING.to_string(),
        settled_status: STATUS_READY.to_string(),
        // Established BY the create of the version it fronts, so its transition is that
        // version's: ready when the runtime is, and transient until then.
        settles_at: version.settles_at,
        // A caller that named no endpoint tagged none either.
        tags: BTreeMap::new(),
        failure_reason: None,
    }
}

impl AgentRuntimeRecord {
    /// Whether this runtime still holds an endpoint a CALLER created.
    ///
    /// The service-managed endpoint does not count: it is this service's to establish and
    /// to remove, so it never stands in the way of an operation the caller asks for on the
    /// runtime itself.
    pub fn holds_caller_created_endpoints(&self) -> bool {
        self.endpoints
            .keys()
            .any(|name| !is_service_managed_endpoint(name))
    }

    /// Move the service-managed endpoint onto `version`, the version this runtime has just
    /// published and which is itself ready at `version_settles_at`, with the `UPDATING` ->
    /// `READY` transition that shift is owed.
    ///
    /// The managed endpoint is the address of whatever the runtime runs now, so a publish
    /// advances it; it keeps serving the version it was serving until the new version is
    /// itself ready, which is when the shift comes due. A caller-created endpoint is
    /// untouched: it pins the version it named.
    ///
    /// A runtime whose managed endpoint is gone has nothing to advance.
    pub fn track_published_version(
        &mut self,
        version: u32,
        version_settles_at: SystemTime,
        now: SystemTime,
    ) {
        let Some(endpoint) = self.endpoints.get_mut(DEFAULT_RUNTIME_ENDPOINT) else {
            return;
        };
        // What the endpoint is serving when the shift starts: the version it points at once
        // its previous transition is due, and until then whatever it was serving before —
        // nothing at all for an endpoint whose create the clock still owes a transition
        // for, which has never served a version to report. The same reading
        // [`AgentRuntimeEndpointRecord::live_version_at`] reports it by.
        endpoint.live_version = if now < endpoint.settles_at {
            endpoint.live_version
        } else {
            Some(endpoint.target_version)
        };
        endpoint.target_version = version;
        endpoint.last_updated_at = now;
        endpoint.transient_status = STATUS_UPDATING.to_string();
        endpoint.settled_status = STATUS_READY.to_string();
        endpoint.settles_at = version_settles_at.max(now);
        // A shift the clock drives to READY has no failure to report.
        endpoint.failure_reason = None;
    }
}
