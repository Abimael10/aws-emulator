// SPDX-License-Identifier: CC0-1.0

//! The RESOURCE-POLICY surface's own cross-service seam: the stored policy document going
//! to the service that ENFORCES it.
//!
//! A CloudTrail Lake channel's resource-based policy is this control plane's document —
//! `PutResourcePolicy` attaches and replaces it, `DeleteResourcePolicy` detaches it — but
//! the call it authorizes is the data plane's: `cloudtrail-data:PutAuditEvents`, whose
//! `externalId` is "conditionally required when the channel's resource policy includes an
//! external ID". So the call that WRITES the policy is what carries it to that service, in
//! the same request, and this module is where that write lives — beside the registry the
//! documents are stored in, for the same reason [`super::channels`] holds the channel
//! identifier reading: both operations that write such a document drive it, and two copies
//! of the seam would drift.
//!
//! The DOCUMENT is what travels. What it says about one ingest is decided where that ingest
//! is served, through the shared IAM evaluator
//! ([`::service_common::iam_authorization::key_value_satisfies_grants`]), so no service here
//! carries a reading of a policy of its own.

/// Give the channel's DATA PLANE the resource policy this control plane just attached to it
/// — or `None` for the policy it just detached — so the `externalId` a `PutAuditEvents`
/// request carries is held to that policy.
///
/// A caller that has just attached a policy including an external ID must present one on the
/// very next ingest, and one that has just detached it no longer needs to: there is no
/// propagation step in between, exactly as there is none for the channel itself
/// ([`super::channels::register_channel_in_data_plane`]).
///
/// `Some(true)` once that registry carries the policy, `Some(false)` when it holds no channel
/// under this ARN (an ingest onto it is refused as absent anyway, so it carries no policy)
/// and `None` when it could not be written at all — the same answer
/// [`super::channels::deregister_channel_in_data_plane`] gives. Best-effort for the reason
/// those two are not: the policy belongs to THIS service's registry, which stores and reports
/// it whatever the data plane holds.
///
/// LOCK ORDER: the data plane takes only ITS own store's lock inside this call, so this must
/// be called with NO guard of this service's store held — which keeps the cross-service write
/// one-directional and a lock cycle between the two stores unreachable.
pub fn apply_channel_resource_policy_in_data_plane<Eng: Send + Sync + 'static>(
    engine: &Eng,
    channel_arn: &str,
    policy: Option<&str>,
) -> Option<bool> {
    ::service_com_amazonaws_cloudtraildata_cloudtraildataservice::state::set_channel_resource_policy(
        engine,
        channel_arn,
        policy,
    )
}
