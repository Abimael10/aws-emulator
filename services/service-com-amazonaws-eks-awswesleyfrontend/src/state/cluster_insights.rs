// SPDX-License-Identifier: CC0-1.0

//! The cluster INSIGHTS slice of this service's refusals: the internal plane that
//! answers for a cluster's insights, and the shape a refusal it answered reaches
//! the caller in.
//!
//! The insights themselves are not stored: an insight is the result of a check
//! Amazon EKS runs against a cluster, evaluated by
//! [`ComAmazonawsEksAwswesleyfrontendState::cluster_insights`]. What lives here is
//! how the plane behind those checks words a read it cannot serve.

use super::*;

/// The internal service that answers for a cluster's INSIGHTS.
///
/// `ListInsights` and `DescribeInsight` are served out of that plane rather than by
/// the Amazon EKS control plane itself, so a read it cannot resolve comes back as
/// that plane's own refusal, carried through WRAPPED — naming the service, the
/// status it answered with, the id of that call and how many attempts were made
/// ([`wrapped_peer_tail`]), exactly as the certificate-authority plane's reads are
/// ([`CERTIFICATE_AUTHORITY_MANAGER`]).
pub const CLUSTER_INSIGHTS_MANAGER: &str = "EksClusterInsights";

/// The HTTP status that plane answers an insight it does not hold with, which the
/// wrapped tail reports. It is the status of the RELAYED call, which is why it is
/// stated in the message rather than left to the status of the answer this service
/// gives its own caller.
pub(super) const INSIGHT_NOT_FOUND_STATUS: u16 = 404;

/// The HTTP status that plane answers a refresh it cannot begin because one is
/// already running with: a request that conflicts with the state of the resource,
/// which HTTP states as `409 Conflict`.
///
/// Like [`INSIGHT_NOT_FOUND_STATUS`] it is the status of the RELAYED call, not of
/// the answer this service gives its own caller — `StartInsightsRefresh` models
/// `InvalidRequestException` (400) for this refusal, so the two differ and the
/// relayed one is readable only in the tail.
pub(super) const REFRESH_IN_PROGRESS_STATUS: u16 = 409;

/// The sentence the insights plane words a refresh it will not begin because the
/// cluster is already running one with.
///
/// Entirely FIXED text: the plane refuses the one refresh slot the cluster has, and
/// the request named the cluster in its own `@httpLabel`, so nothing of the request
/// or of the running refresh varies in it.
pub(super) const INSIGHTS_REFRESH_IN_PROGRESS_SENTENCE: &str =
    "Another refresh operation is already in progress for the cluster.";

/// The message a second `StartInsightsRefresh` is refused with while the cluster's
/// refresh still runs: the insights plane's own sentence
/// ([`INSIGHTS_REFRESH_IN_PROGRESS_SENTENCE`]) followed by the tail naming the call
/// it came from ([`wrapped_peer_tail`]), exactly as a read that plane could not
/// serve is worded ([`insight_not_found`]).
///
/// The request id in the tail is drawn afresh per call, because that is what a
/// request id IS — it identifies the call that was made, and nothing in the request
/// decides it.
#[must_use]
pub fn insights_refresh_in_progress_message() -> String {
    format!(
        "{INSIGHTS_REFRESH_IN_PROGRESS_SENTENCE}{}",
        wrapped_peer_tail(CLUSTER_INSIGHTS_MANAGER, REFRESH_IN_PROGRESS_STATUS)
    )
}

/// The sentence the insights plane words for an insight id it cannot resolve
/// against the cluster the read named.
///
/// It states the two identifiers of the analysis that was asked for as `key=value`
/// pairs: the insight id the request carried, and the id of the CLUSTER — the id
/// the cluster publishes for itself ([`derive_cluster_resource_id`]) rather than
/// the name the request addressed it with, because the plane behind the check knows
/// the cluster by that id.
pub fn insight_not_found_message(insight_id: &str, cluster_id: &str) -> String {
    format!("Cannot find insight insightId={insight_id} clusterId={cluster_id}")
}

/// The `ResourceNotFoundException` a read of an insight the named cluster does not
/// hold is refused with: the insights plane's own sentence
/// ([`insight_not_found_message`]) followed by the tail naming the call it came from
/// ([`wrapped_peer_tail`]).
///
/// NO identity member is set, the rule every child-resource read of this service
/// follows ([`child_resource_not_found`]): the INSIGHT is the resource reported
/// missing, and the exception models no member for one. The request id in the tail
/// is drawn afresh per call, because that is what a request id IS — it identifies
/// the call that was made, and nothing in the request decides it.
pub fn insight_not_found(insight_id: &str, cluster_id: &str) -> ResourceNotFoundException {
    child_resource_not_found(format!(
        "{}{}",
        insight_not_found_message(insight_id, cluster_id),
        wrapped_peer_tail(CLUSTER_INSIGHTS_MANAGER, INSIGHT_NOT_FOUND_STATUS)
    ))
}
