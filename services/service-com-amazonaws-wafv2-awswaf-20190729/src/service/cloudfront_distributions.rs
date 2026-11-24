// SPDX-License-Identifier: CC0-1.0

//! The CloudFront DISTRIBUTIONS a web ACL is still in front of, resolved with CloudFront.
//!
//! # The rule
//!
//! A CloudFront distribution is the one protected resource that is NOT bound through
//! `AssociateWebACL`: "for Amazon CloudFront, don't use this call. Instead, use your
//! CloudFront distribution configuration" — the association lives in the distribution's own
//! `WebACLId`, so this service holds no record of it. `DeleteWebACL` says the same thing
//! about where to look: "before deleting any web ACL, first disassociate it from all
//! resources. To retrieve a list of the resources that are associated with a web ACL …
//! for Amazon CloudFront distributions, use the CloudFront call
//! ListDistributionsByWebACLId". So the distributions a web ACL still protects are
//! CloudFront's answer and nothing else.
//!
//! # How the question is asked
//!
//! Through that one read, over the AWS SDK driven on the emulator's own in-process
//! transport via the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]): no socket is opened, no name is
//! resolved, nothing leaves this process, and this crate names no other service's crate —
//! which is also why asking cannot form a dependency cycle with CloudFront, which asks this
//! service for its edge decisions ([`crate::service::edge`]). The credentials are the
//! callback's, which sign that in-process hop and never appear in a response.
//!
//! The Region asked in is the one the web ACL's own ARN carries, read as a Region LABEL
//! ([`::service_common::arn::region_label`]) because it ends up in an endpoint URL's
//! authority; a global-scope web ACL's ARN carries the one Region CloudFront's control
//! plane is addressed in, so that is also the fallback for an ARN carrying no label.
//!
//! # What an unanswered question means
//!
//! Only an answer that REPORTS a distribution is a protection: an emulator with no
//! CloudFront mounted has no one to ask, so this reports no distribution rather than
//! inventing one, and the call proceeds as the caller asked. The only claim this module
//! makes is one CloudFront made.

/// The Region CloudFront's control plane is addressed in, which is also the Region a
/// `CLOUDFRONT`-scope web ACL's ARN carries ("to work with CloudFront, you must also specify
/// the Region US East (N. Virginia)"). The fallback for an ARN whose Region field is no
/// label an endpoint authority can carry.
const CLOUDFRONT_REGION: &str = "us-east-1";

/// The endpoint prefix CloudFront's control plane is addressed by.
const CLOUDFRONT_VENDOR: &str = "cloudfront";

/// Whether CloudFront reports at least one distribution whose configuration still names the
/// web ACL `web_acl_arn`.
///
/// `false` when CloudFront reports none, and `false` when nothing answered at all — see this
/// module's own documentation for why those are the same decision here.
///
/// Call it with NO state guard held: it awaits.
pub async fn protected_distributions(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    web_acl_arn: &str,
) -> bool {
    let region = ::service_common::arn::parse(web_acl_arn)
        .and_then(|arn| ::service_common::arn::region_label(arn.region))
        .unwrap_or(CLOUDFRONT_REGION);
    let listed = client(ctx, region)
        .list_distributions_by_web_acl_id()
        .web_acl_id(web_acl_arn)
        .send()
        .await;
    match listed {
        // "The ID of the WAF web ACL that you want to list the associated distributions …
        // for WAFV2, this is the ARN of the web ACL": the page CloudFront answers with holds
        // exactly the distributions whose stored configuration names it, so a page with any
        // item in it is CloudFront reporting the web ACL still in front of something.
        Ok(listed) => listed
            .distribution_list()
            .is_some_and(|list| !list.items().is_empty()),
        Err(error) => {
            // At `debug`: an emulator that mounts no CloudFront answers every such hop this
            // way, which is a property of the deployment rather than of the request.
            ::tracing::debug!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "CloudFront did not answer which distributions this web ACL is in front of"
            );
            false
        }
    }
}

/// A CloudFront client over the in-process callback into this same emulator, addressed to
/// `region`'s endpoint.
///
/// The endpoint URL is only the address the callback's transport routes on IN PROCESS. The
/// transport being in process is also why there is nothing to retry, time out or watch for a
/// stalled stream — each would need a sleep implementation this client is not given — and
/// why the credentials the callback hands over are used directly rather than cached.
fn client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_cloudfront::Client {
    let config = ::aws_sdk_cloudfront::Config::builder()
        .behavior_version(::aws_sdk_cloudfront::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_cloudfront::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{CLOUDFRONT_VENDOR}.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_cloudfront::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudfront::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudfront::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudfront::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudfront::Client::from_conf(config)
}
