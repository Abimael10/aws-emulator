// SPDX-License-Identifier: CC0-1.0

//! CONTINUOUS DEPLOYMENT: WHICH of a continuous deployment's two distributions serves one
//! viewer request.
//!
//! A distribution's `ContinuousDeploymentPolicyId` attaches "the parameters for routing
//! production traffic from your primary to staging distributions": while that policy is
//! `Enabled`, a viewer request addressed to the PRIMARY distribution's own domain name is
//! served by the STAGING distribution the policy names whenever the policy's `TrafficConfig`
//! selects that request, and by the primary otherwise. The viewer addresses one name either
//! way — the staging distribution's own domain is not what the request carries — so this is a
//! decision the edge makes about which stored configuration serves the request it has already
//! resolved: the behaviours, the cache and the origins of whichever of the two is serving.
//!
//! `TrafficConfig$Type` names which of the two configurations selects the request:
//!
//! * `SingleHeader` — "if the HTTP request contains a header and value that matches what you
//!   specify here, the request is sent to the staging distribution. Otherwise the request is
//!   sent to the primary distribution", read off the request itself
//!   ([`carries_header`]);
//! * `SingleWeight` — "the percentage of traffic to send to a staging distribution", which is
//!   a share of the SEQUENCE of requests the policy covers and so is decided by the policy's
//!   own record ([`crate::state::StoredContinuousDeploymentPolicy::routes_weighted_request`]),
//!   once per viewer session where the configuration names session stickiness.
//!
//! The decision is made ONCE per request, before anything else the edge does with it, so one
//! request is served by one distribution throughout: its cache behaviour, the functions that
//! behaviour associates, the web ACL in front of it, the origin fetched and the series the
//! answer is counted into are all the serving distribution's.

use aws_smithy_runtime_api::http::Request as RtRequest;
use std::time::SystemTime;

use super::generated_headers;
use crate::state::{
    ComAmazonawsCloudfrontCloudfront20200531State, TRAFFIC_TYPE_SINGLE_HEADER,
    TRAFFIC_TYPE_SINGLE_WEIGHT,
};

/// The id of the STAGING distribution this viewer request is routed to, or `None` when the
/// distribution the request addresses serves it itself.
///
/// Takes the store mutably because a `SingleWeight` configuration routes a SHARE of the
/// requests it covers: serving one request is what moves that share on, and what opens a
/// viewer session where the configuration holds sessions to one side. A `SingleHeader`
/// configuration moves nothing — it reads the request — and neither does a request no policy
/// covers.
pub(super) fn staging_distribution(
    store: &mut ComAmazonawsCloudfrontCloudfront20200531State,
    req: &RtRequest,
    now: SystemTime,
) -> Option<String> {
    let addressed = super::Addressed::of_request(req);
    // Resolved under the same rule the serving path resolves it by, so the distribution whose
    // policy is read is the one the request is really addressed to.
    let (policy_id, staging_id) = {
        let resolved = super::resolve(store, &addressed)?;
        let primary = resolved.distribution;
        // Only a PRIMARY routes: the parameters are for "routing production traffic from your
        // primary to staging distributions", so a request addressed to a staging distribution
        // is served by it. A distribution that accepts no user requests at all routes none
        // either — a disabled distribution serves nothing, whatever a policy it names says.
        if primary.config.staging == Some(true) || !primary.config.enabled {
            return None;
        }
        let policy_id = primary
            .config
            .continuous_deployment_policy_id
            .as_deref()
            .filter(|id| !id.is_empty())?
            .to_string();
        let policy = store.continuous_deployment_policy(&policy_id)?;
        // `ContinuousDeploymentPolicyConfig$Enabled`: "When this value is `true`, this policy
        // is enabled and in effect. When this value is `false`, this policy is not enabled and
        // has no effect."
        if !policy.config.enabled {
            return None;
        }
        // The staging distribution the policy names, by "the CloudFront domain name of the
        // staging distribution": resolved against this account's own records, so a policy
        // naming a domain no distribution of this account publishes routes nothing — there is
        // no second distribution to serve the request.
        let staging_id = policy
            .staging_distribution_dns_names()
            .iter()
            .find_map(|name| store.distribution_by_domain_name(name))
            .filter(|staging| staging.id != primary.id)
            .map(|staging| staging.id.clone())?;
        (policy_id, staging_id)
    };
    // `TrafficConfig` is optional, and it is what routes traffic: a policy that names none
    // sends every request to the primary.
    let traffic = store
        .continuous_deployment_policy(&policy_id)?
        .config
        .traffic_config
        .clone()?;
    if traffic.r#type == TRAFFIC_TYPE_SINGLE_HEADER {
        let config = traffic.single_header_config.as_ref()?;
        return carries_header(req, &config.header, &config.value).then_some(staging_id);
    }
    if traffic.r#type == TRAFFIC_TYPE_SINGLE_WEIGHT {
        let config = traffic.single_weight_config.as_ref()?;
        // The viewer this request came from, read where every other reader of its address
        // reads it, so a session is the same viewer's wherever it is consulted.
        let viewer = generated_headers::viewer_address(req);
        let policy = store.continuous_deployment_policies.get_mut(&policy_id)?;
        return policy
            .routes_weighted_request(
                config.weight,
                config.session_stickiness_config.as_ref(),
                viewer,
                now,
            )
            .then_some(staging_id);
    }
    // A `Type` this emulator does not know selects nothing, so the primary serves the request.
    None
}

/// Whether `req` carries the header a `SingleHeaderConfig` names, with the value it names.
///
/// "If the HTTP request contains a header and value that matches what you specify here, the
/// request is sent to the staging distribution." A header name is case-insensitive (RFC 9110),
/// so the name is compared without regard to case, while the VALUE is compared exactly — it is
/// the value the configuration states, and a header value is case-sensitive.
fn carries_header(req: &RtRequest, header: &str, value: &str) -> bool {
    req.headers()
        .iter()
        .any(|(name, stated)| name.eq_ignore_ascii_case(header) && stated == value)
}

/// The resolution one viewer request is SERVED under, given the staging distribution (if any)
/// a continuous deployment policy routed it to.
///
/// The viewer path and the distribution the viewer ADDRESSED are the request's own either way;
/// what the routing decides is WHICH stored configuration answers it.
pub(super) fn serving<'s, 'r>(
    store: &'s ComAmazonawsCloudfrontCloudfront20200531State,
    resolved: super::Resolved<'s, 'r>,
    staging: Option<&str>,
) -> super::Resolved<'s, 'r> {
    match staging.and_then(|id| store.distribution(id)) {
        Some(distribution) => super::Resolved {
            distribution,
            addressed: resolved.addressed,
            viewer_path: resolved.viewer_path,
        },
        None => resolved,
    }
}
