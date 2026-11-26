// SPDX-License-Identifier: CC0-1.0

//! The resource metrics configurations this store holds, and which resources can have one.
//!
//! A resource metrics configuration "enables detailed metric collection for the resource that is
//! identified by its Amazon Resource Name (ARN). Each Amazon Web Services resource can have only
//! one resource metrics configuration."
//! (`smithy_query(query="shape",shape="ResourceMetricsConfiguration")`) — so a configuration is
//! held under the ARN of the resource it applies to
//! ([`super::ComAmazonawsCloudwatchGraniteserviceversion20100801State::resource_metrics_configurations`]),
//! and the resource it applies to has to be one whose detailed metrics this service collects at
//! all. Which resources those are is the rule this module holds
//! ([`collects_detailed_metrics`], read off the ONE shared ARN parse `service_common::arn`), read
//! by every operation that reports, writes or removes such a configuration: a request naming any
//! other resource is refused with [`UNSUPPORTED_RESOURCE_TYPE`] rather than answered about a
//! configuration that could never exist.

use super::ComAmazonawsCloudwatchGraniteserviceversion20100801State as State;
use crate::ops::types::ResourceMetricsConfiguration;

/// What a resource metrics configuration operation refuses an ARN this service collects no
/// detailed metrics for with: the request-validation class in CloudWatch's own wording
/// ([`crate::scaffolded::Scaffolded::validation_error`], HTTP 400 `ValidationError`).
///
/// One sentence for every such operation, so a reader, a writer and a removal cannot word the
/// same refusal differently.
pub const UNSUPPORTED_RESOURCE_TYPE: &str = "Unsupported resource type";

/// The `ResourceNotFoundException$ResourceType` a refusal names a resource metrics configuration
/// that does not exist by — the kind of resource that was looked up, as the shape the model
/// declares for it is named.
pub const RESOURCE_METRICS_CONFIGURATION: &str = "ResourceMetricsConfiguration";

/// The resource types this service collects DETAILED metrics for, each as the
/// `(vendor, resource type)` pair the ARN of one such resource carries — the service namespace
/// that owns it, and the token its resource field names the type with
/// (`::service_common::arn::Arn::resource_type`).
///
/// A resource metrics configuration "enables detailed metric collection for the resource"
/// (`smithy_query(query="shape",shape="ResourceMetricsConfiguration")`), naming the metrics
/// CloudWatch is to gather for it ("The names of the metrics to collect for the resource. Amazon
/// CloudWatch collects only the metrics that you list here.",
/// `smithy_query(query="shape",shape="ResourceMetricSelection",member="IncludeMetrics")`) — so a
/// resource that can hold one is a resource CLOUDWATCH ITSELF collects metrics for when asked,
/// the container clusters whose per-cluster detailed metric collection is turned on through
/// CloudWatch: an Amazon ECS cluster and an Amazon EKS cluster.
///
/// A resource whose extra metric collection is switched on where the resource LIVES rather than
/// through a CloudWatch configuration — the per-resource one-minute monitoring an Amazon EC2
/// instance or an Amazon EBS volume is put into through Amazon EC2 — is NOT one of these, and
/// that EXCLUSION is the pinned half of this set: the service is observed refusing a well-formed
/// ARN of such a resource under [`UNSUPPORTED_RESOURCE_TYPE`] rather than answering about a
/// configuration of it, so that collection is not a resource metrics configuration and this
/// service holds none for such a resource.
///
/// The MEMBERSHIP is provisional. The model publishes no list of covered types — `ResourceArn` is
/// constrained by length and the ARN pattern alone
/// (`smithy_query(query="shape",shape="ResourceArn")`) — and no documented list is reachable from
/// this crate, so the entries above are this emulator's reading of which resources CloudWatch
/// itself collects metrics for on request, not a published catalog. They are what makes the two
/// answers distinguishable at all: the set is ENUMERATED here rather than an uncovered type being
/// reported as a configuration that does not exist, because the service judges the TYPE of
/// resource before it looks a configuration up, so "no detailed metrics are collected for this
/// type" and "this resource has no configuration" are different answers and only the first can be
/// given about a type at all. An entry is added when a type's coverage is observed, and removed
/// when a refusal of it is — one edit here, which every operation that reports, writes or removes
/// a configuration follows.
pub const DETAILED_METRIC_RESOURCE_TYPES: [(&str, &str); 2] =
    [("ecs", "cluster"), ("eks", "cluster")];

/// Whether this service collects detailed metrics for the resource `value` names — the one
/// question every resource metrics configuration operation asks about the ARN it was given,
/// before it reads or writes anything.
///
/// The ARN is read through the ONE shared parse (`service_common::arn`), the same one the alarm
/// actions and the managed rules are read with, so this service cannot disagree with itself
/// about what an ARN names. Two things have to hold: the value is a well-formed ARN whose vendor
/// owns one of the types above, and its resource field names a resource OF that type — a type
/// token with a non-empty id after it, since a resource field carrying a type alone addresses no
/// resource whose metrics could be collected.
#[must_use]
pub fn collects_detailed_metrics(value: &str) -> bool {
    let Some(arn) = ::service_common::arn::parse(value) else {
        return false;
    };
    DETAILED_METRIC_RESOURCE_TYPES
        .iter()
        .any(|&(vendor, resource_type)| {
            arn.vendor == vendor
                && ::service_common::arn::resource_id_of_type(value, resource_type)
                    .is_some_and(|id| !id.is_empty())
        })
}

impl State {
    /// The resource metrics configuration this store holds for `resource_arn`, or `None` when it
    /// holds none — which is what a reader answers `ResourceNotFoundException` for.
    ///
    /// A pure read: the configuration is reported exactly as the operation that wrote it left it.
    #[must_use]
    pub fn resource_metrics_configuration(
        &self,
        resource_arn: &str,
    ) -> Option<&ResourceMetricsConfiguration> {
        self.resource_metrics_configurations.get(resource_arn)
    }

    /// Hold `configuration` as the configuration of the resource it names.
    ///
    /// The one write of this store, so a writer cannot key a configuration differently from the
    /// way [`Self::resource_metrics_configuration`] looks one up: the key is the ARN the
    /// configuration itself carries, and a second write for the same resource replaces the
    /// configuration held for it rather than adding a second one.
    pub fn keep_resource_metrics_configuration(
        &mut self,
        configuration: ResourceMetricsConfiguration,
    ) {
        self.resource_metrics_configurations
            .insert(configuration.resource_arn.clone(), configuration);
    }
}
