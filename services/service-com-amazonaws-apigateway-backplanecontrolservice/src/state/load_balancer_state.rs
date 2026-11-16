// SPDX-License-Identifier: CC0-1.0

//! Whether the NETWORK LOAD BALANCER a VPC link is being provisioned over EXISTS, for the
//! precondition that provisioning turns on every entry of the link's `targetArns`.
//!
//! `targetArns` is "The ARN of the network load balancer of the VPC targeted by the VPC
//! link. The network load balancer must be owned by the same Amazon Web Services account of
//! the API owner" (`VpcLink$targetArns`), so each entry names a load balancer the link has
//! to CARRY TRAFFIC THROUGH. A load balancer that does not exist carries nothing: a link
//! provisioned over one could never deliver a private integration's request, so the link
//! cannot reach `AVAILABLE` and settles `FAILED` instead ([`super::VpcLinkEntry::failure`]).
//! Whether a load balancer exists is not a fact this service holds, though — a load balancer
//! is Elastic Load Balancing's resource, and its `DescribeLoadBalancers` reports the ones
//! the account has.
//!
//! It is therefore ASKED there, the way any emulated service reaches another — through the
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`]), over the
//! client below — so no socket is opened and nothing leaves the process. Nothing is cached:
//! a load balancer created a moment ago is weighed at once, and one just deleted stops being
//! resolvable at once.
//!
//! WHAT THIS GATE READS IS AN EXISTENCE ANSWER, and nothing else. A target Elastic Load
//! Balancing answers "no such load balancer" about does not resolve; an answer that
//! establishes nothing — a hop that could not be made, any other failure of that service —
//! establishes nothing, so it fails no provisioning and says so at `warn`. That is the same
//! reading of silence [`super::acm_certificate_state`] and [`super::vpc_endpoints`] take,
//! and the outcome that IS settled is the one the owner's answer grounds.
//!
//! The per-crate client below is this tree's convention for that hop, for the reason those
//! modules record: each service asks a different question and takes its own reading of
//! silence. What is shared is already shared — the Region-label grammar and the ARN parse
//! ([`::service_common::arn`]).

/// The `statusMessage` of a VPC link whose provisioning FAILED because a target ARN names a
/// network load balancer that does not exist.
///
/// FIXED TEXT: API Gateway states this whole sentence for an unresolvable target and carries
/// no part of the request in it — not the target ARN, not the link's id and not its name — so
/// two links failed for this reason are described identically. It is a DIFFERENT sentence
/// from the one a target that names no network load balancer at all earns
/// ([`super::vpc_link_target_is_not_a_load_balancer`]): that is a question about the ARN, this
/// one is about the resource it names.
pub const NLB_ARN_NOT_FOUND: &str = "NLB ARN not found";

/// The AWS service whose endpoint the load balancers of an account are described from, and
/// the vendor field their ARNs carry: both are `elasticloadbalancing` for either Elastic Load
/// Balancing API (`smithy_query(query="service_info")` on that service's model reports
/// `endpointPrefix`, `arnNamespace` and the sigv4 name alike).
const LOAD_BALANCING_VENDOR: &str = "elasticloadbalancing";

/// How many DISTINCT targets one link is resolved for.
///
/// One hop per target, so the work a single call costs is capped by a constant rather than by
/// how many targets the caller listed. It is the ceiling the create already holds
/// `targetArns` to ([`super::MAX_COLLECTION_ENTRIES`]), so no target a link can actually
/// carry goes unresolved.
const MAX_TARGETS_RESOLVED: usize = super::MAX_COLLECTION_ENTRIES;

/// An AWS SDK client for Elastic Load Balancing over the in-process callback into this same
/// emulator.
///
/// `region` is the Region the target's own ARN names, so the question is asked where the load
/// balancer lives. The endpoint URL is only the address the callback's transport routes on IN
/// PROCESS. The credentials are the callback's, which sign this in-process hop and never
/// appear in a response.
///
/// A Region read off a caller-supplied ARN is not just data here — it ends up in an endpoint
/// URL's authority — so it is read as a Region LABEL ([`::service_common::arn::region_label`],
/// the one place this tree states that grammar) instead of being interpolated as given:
/// `None` for a value that would name an authority the caller chose rather than the peer
/// service this emulator asks.
fn elastic_load_balancing(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_elasticloadbalancingv2::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_elasticloadbalancingv2::Config::builder()
        .behavior_version(::aws_sdk_elasticloadbalancingv2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_elasticloadbalancingv2::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://{LOAD_BALANCING_VENDOR}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials the callback
        // hands over are used directly.
        .retry_config(::aws_sdk_elasticloadbalancingv2::config::retry::RetryConfig::disabled())
        .timeout_config(
            ::aws_sdk_elasticloadbalancingv2::config::timeout::TimeoutConfig::disabled(),
        )
        .identity_cache(::aws_sdk_elasticloadbalancingv2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_elasticloadbalancingv2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    Some(::aws_sdk_elasticloadbalancingv2::Client::from_conf(config))
}

/// What the owner of load balancers answered about the one a target named.
enum Answer {
    /// Nothing was established — evidence of nothing (see this module's documentation).
    Unknown,
    /// Whether the load balancer asked about is one Elastic Load Balancing holds.
    Exists(bool),
}

/// Ask Elastic Load Balancing for the load balancer `target_arn` names.
async fn load_balancer_exists(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    target_arn: &str,
) -> Answer {
    // The load balancer's OWN Region, and the Region the caller addressed for a target whose
    // ARN names none — which is the Region a load balancer of this account would be held in
    // anyway.
    let region =
        ::service_common::arn::region_of(target_arn, LOAD_BALANCING_VENDOR).unwrap_or(request_region);
    let Some(client) = elastic_load_balancing(ctx, region) else {
        ::tracing::warn!(
            "neither the target nor the request names a Region label, so elastic load \
             balancing cannot be asked"
        );
        return Answer::Unknown;
    };
    match client
        .describe_load_balancers()
        .load_balancer_arns(target_arn)
        .send()
        .await
    {
        // The one load balancer asked about, as that service reports it: an answer carrying
        // it is the resource existing, and an answer carrying none establishes nothing (that
        // service answers its modelled not-found instead, read below).
        Ok(answer) => match answer.load_balancers().is_empty() {
            false => Answer::Exists(true),
            true => Answer::Unknown,
        },
        Err(error) => {
            // The modelled "The specified load balancer does not exist"
            // (`LoadBalancerNotFoundException` of `DescribeLoadBalancers`) is the owner
            // saying the account holds no such load balancer — the one answer this gate acts
            // on.
            if error
                .as_service_error()
                .is_some_and(::aws_sdk_elasticloadbalancingv2::operation::describe_load_balancers::DescribeLoadBalancersError::is_load_balancer_not_found_exception)
            {
                return Answer::Exists(false);
            }
            // At `warn`: this gate fails nothing on an answer that establishes nothing, so an
            // emulator where the hop cannot be made provisions a link AWS would not, and that
            // is worth seeing. The ARN is metadata of the request, not a secret.
            ::tracing::warn!(
                target_arn = %target_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "elastic load balancing did not report whether the target exists"
            );
            Answer::Unknown
        }
    }
}

/// Resolve every target a VPC link would be provisioned over against the load balancers
/// Elastic Load Balancing holds, and answer the `statusMessage` the link settles `FAILED`
/// with — or `None` for a link whose provisioning this gate does not fail.
///
/// `target_arns` are the entries the link carries, in the order the request listed them, so
/// the outcome does not depend on which target was asked about first. A blank entry names no
/// load balancer and is skipped, and one target named twice is asked about once.
///
/// `None` for a link every target of which Elastic Load Balancing reports, and for one no
/// answer could be established for (see this module's documentation). `Some` carries
/// [`NLB_ARN_NOT_FOUND`], the `statusMessage` of a link that could not be provisioned.
pub async fn unresolvable_target<'arn, I>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    target_arns: I,
) -> Option<String>
where
    I: IntoIterator<Item = &'arn str>,
{
    let mut asked: Vec<&str> = Vec::new();
    for target_arn in target_arns {
        let target_arn = target_arn.trim();
        if target_arn.is_empty() || asked.contains(&target_arn) {
            continue;
        }
        if asked.len() >= MAX_TARGETS_RESOLVED {
            // At `warn`, for the same reason an unestablished answer is logged: past the
            // ceiling this call weighs nothing more, so a link naming more distinct targets
            // than a create admits is provisioned on an unresolved tail.
            ::tracing::warn!(
                asked = asked.len(),
                "more distinct targets were named than one link can carry, so the rest were \
                 not weighed"
            );
            break;
        }
        asked.push(target_arn);
        match load_balancer_exists(ctx, request_region, target_arn).await {
            Answer::Unknown | Answer::Exists(true) => {}
            Answer::Exists(false) => return Some(NLB_ARN_NOT_FOUND.to_string()),
        }
    }
    None
}
