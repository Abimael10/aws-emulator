// SPDX-License-Identifier: CC0-1.0

//! The account's EBS volume ATTACHMENTS: another fact this service RESOLVES
//! rather than holds.
//!
//! A component of an application covers a resource and the storage that resource
//! runs on: `CreateComponent` "creates a custom component by grouping similar
//! standalone instances to monitor"
//! (`smithy_query(query="operation",operation="CreateComponent")`), and the
//! volumes attached to an instance are part of that instance rather than
//! resources of their own to monitor beside it. Which volumes those are is Amazon
//! EC2's to answer, not this service's, so it is ASKED — over the same in-process
//! callback the application's resource group is resolved through
//! ([`super::resource_groups`]).
//!
//! Failing to ask reports no attachment, exactly as an account with no volume
//! does: an emulator with no Amazon EC2 mounted knows of no volume to carry into
//! a component, and nothing is invented in its place.

use std::collections::BTreeMap;

/// The Amazon EC2 client this service reads the account's volume attachments
/// through.
///
/// Built like [`super::resource_groups`]'s client and for the same reasons: the
/// transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream, the credentials the callback hands over are used directly, and
/// `region` is the request's own signing region, because a volume lives in the
/// region its instance does.
fn ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_ec2::Client {
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// How many pages of the account's volumes are read.
///
/// The listing is paged, and the pages are followed until Amazon EC2 stops
/// handing back a token — this is what bounds that walk, so a token that never
/// runs out cannot loop. It covers far more volumes than the resources one
/// application's components hold ([`super::COMPONENT_QUOTA`] components of at
/// most [`super::COMPONENT_RESOURCE_QUOTA`] resources each).
const MAX_VOLUME_PAGES: usize = 32;

/// Which EBS volumes each of the account's instances holds in `region`: instance
/// ARN -> the ARNs of the volumes attached to it, in volume-ARN order.
///
/// Read in ONE walk of the account's volumes rather than once per instance, so
/// what a caller resolving a whole group's membership costs is bounded by the
/// account's volumes instead of by the number of resources the group holds.
///
/// An instance the account holds no attached volume for is absent from the map,
/// which is the same answer an emulator with no Amazon EC2 mounted gives.
pub async fn attached_volumes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> BTreeMap<String, Vec<String>> {
    let client = ec2(ctx, region);
    let mut volumes_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_VOLUME_PAGES {
        let answer = client
            .describe_volumes()
            .set_next_token(next_token.take())
            .send()
            .await;
        let page = match answer {
            Ok(page) => page,
            Err(error) => {
                let reported = ::aws_smithy_types::error::display::DisplayErrorContext(&error);
                if error.as_service_error().is_some() {
                    // Amazon EC2 ANSWERED a failure: it holds the account's
                    // volumes and could not report them, which is worth a
                    // developer's attention — a component then covers its
                    // instance alone.
                    ::tracing::warn!(
                        error = %reported,
                        "Amazon EC2 refused to report the account's volumes"
                    );
                } else {
                    // Nobody answered at all: an emulator with no Amazon EC2
                    // mounted knows of no volume, which is not a failure of this
                    // request.
                    ::tracing::debug!(
                        error = %reported,
                        "no Amazon EC2 answered for the account's volumes"
                    );
                }
                break;
            }
        };
        for volume in page.volumes() {
            let Some(volume_id) = volume.volume_id() else {
                continue;
            };
            for attachment in volume.attachments() {
                let Some(instance_id) = attachment.instance_id() else {
                    continue;
                };
                volumes_of
                    .entry(instance_arn(region, instance_id))
                    .or_default()
                    .push(volume_arn(region, volume_id));
            }
        }
        next_token = page
            .next_token()
            .map(str::to_string)
            .filter(|token| !token.is_empty());
        if next_token.is_none() {
            break;
        }
    }
    for volumes in volumes_of.values_mut() {
        volumes.sort();
        volumes.dedup();
    }
    volumes_of
}

/// The ARN of the instance `instance_id` names in `region`.
///
/// Minted rather than read off the listing: Amazon EC2 reports an attachment by
/// instance id, and the resources an application onboards are named by ARN, so
/// the two are matched in the one form — the same form that service's own records
/// carry, `arn:<partition>:ec2:<region>:<account>:instance/<id>`.
fn instance_arn(region: &str, instance_id: &str) -> String {
    ::engine::emulator_arn("ec2", region, &format!("instance/{instance_id}"))
}

/// The ARN of the volume `volume_id` names in `region`, minted in the same form
/// and for the same reason as [`instance_arn`].
fn volume_arn(region: &str, volume_id: &str) -> String {
    ::engine::emulator_arn("ec2", region, &format!("volume/{volume_id}"))
}

/// The resources one DISCOVERED component covers for `resource_arn`: that
/// resource, followed by the volumes attached to it when it is an EC2 instance,
/// once each.
///
/// The one form a component Application Insights onboards — and one a
/// `DeleteComponent` restores — covers an instance in, so a resource that comes
/// back as a standalone component covers exactly what it covered before it was
/// grouped. `CreateComponent` keeps the list the caller grouped as it was given
/// and appends the same attachments to it, so a custom component covers the same
/// resources without rewriting the request.
pub fn with_attached_volumes(
    resource_arn: &str,
    attachments: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut resources = vec![resource_arn.to_string()];
    for volume in attachments.get(resource_arn).into_iter().flatten() {
        if !resources.iter().any(|held| held == volume) {
            resources.push(volume.clone());
        }
    }
    resources
}
