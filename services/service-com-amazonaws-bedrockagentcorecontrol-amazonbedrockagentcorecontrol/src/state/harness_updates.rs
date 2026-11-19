// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE of a harness, or of one of its endpoints, is bounded and refused by: the
//! version ceiling the wire can carry, and the conflict a change asked of an endpoint whose
//! previous change is still in flight earns.
//!
//! Stated here rather than in either handler, so `UpdateHarness` and
//! `UpdateHarnessEndpoint` cannot bound or refuse one resource in two different ways.

use super::*;

/// The highest version number a harness can publish: `HarnessVersion` is
/// `^([1-9][0-9]{0,4})$` (`smithy_query shape=HarnessVersion`), so five digits. A harness
/// that has published this many versions can publish no more — the next number is one no
/// response could report.
pub const MAX_HARNESS_VERSION: u32 = 99_999;

/// How this service words that ceiling being met, in `UpdateHarness`'s modelled
/// `ServiceQuotaExceededException`: the harness the request named and the ceiling it is at,
/// in the form every ceiling of this service is reported in.
pub fn harness_version_quota_met(harness_id: &str) -> String {
    format!("harness {harness_id} has published the maximum of {MAX_HARNESS_VERSION} versions")
}

/// The modelled conflict a change asked of a harness ENDPOINT that is not settled earns:
/// the endpoint the request named and the status it found.
///
/// A harness endpoint reaches `READY` on the clock, so a request arriving while its create —
/// or an earlier shift of the version it serves — is still in flight finds a transient
/// status, and `UpdateHarnessEndpoint` models `ConflictException` (HTTP 409) for exactly
/// that. The sentence is fixed; its variable parts are the endpoint's own name, as the
/// request carries it, and the status it found, spelled as the raw token its enum declares
/// (`smithy_query shape=HarnessEndpointStatus`) — the same form the response members carry
/// it in.
///
/// Stated once rather than per handler, so no write path of this family can disagree about
/// how a busy endpoint is refused.
pub fn harness_endpoint_status_conflict(endpoint_name: &str, status: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "Cannot update endpoint {endpoint_name} while it is {status}. Wait and try again."
        )),
    }
}
