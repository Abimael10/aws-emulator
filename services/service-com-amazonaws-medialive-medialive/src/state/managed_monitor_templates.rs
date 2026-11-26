// SPDX-License-Identifier: CC0-1.0

//! The AWS-PROVIDED monitor template catalog: the cloudwatch alarm template
//! GROUPS the service itself publishes and the TEMPLATES inside them.
//!
//! One file for the whole catalog — the seed rows, the ids and ARNs they are
//! published under, the instant they were published and the records a region's
//! store is seeded with — so the catalog can be read, and merged, on its own,
//! exactly as the [`super::offerings`] catalog beside it is.
//!
//! ## Why this is state and not a response a handler assembles
//!
//! `CloudWatchAlarmTemplateGroupSummary$Id` is "A cloudwatch alarm template
//! group's id. AWS provided template groups have ids that start with `aws-`" and
//! `CloudWatchAlarmTemplateSummary$Id` says the same of a template, so the model
//! itself records that this resource family has two provenances: the ones an
//! account creates and the ones the SERVICE publishes. The published ones exist
//! in every account without anybody creating them — there is no operation that
//! creates one — so they are seeded into each region's partition when it comes
//! up ([`super::ComAmazonawsMedialiveMedialiveState::with_mints`]) and every
//! reader (the listings, the single gets, the `Scope` filter, a group's
//! `TemplateCount`) answers from that one store rather than from a catalog of its
//! own.
//!
//! Because they belong to the service and not to the account, they are READ
//! ONLY: nothing writes a seeded record after the partition is built, and the
//! operations that modify or delete a monitor template resource refuse one whose
//! id carries the AWS-provided prefix ([`is_published_id`]). An account's own
//! template may be
//! created INSIDE a published group — that is the documented starting point for
//! MediaLive monitoring — which is why a published group's `TemplateCount` is
//! still derived from the templates that exist rather than from this table.
//!
//! ## Where the data comes from
//!
//! * The GROUP NAMES are the catalog's own: these are the groups MediaLive
//!   publishes, one per media workflow it ships monitoring for, and a client
//!   resolves a published group by exactly these names.
//! * A published TEMPLATE's NAME is the name the catalog publishes that alarm
//!   under, which is DERIVED rather than stored: the `AWS-` prefix, the metric
//!   the alarm watches followed by the direction a breach of it lies in, then
//!   the workflow family of the group publishing it
//!   (`AWS-<Metric><High|Low>-<Workflow>Workflows`, see
//!   [`PublishedTemplate::name`]). A client resolves a published template by
//!   that name as readily as by its id (`Identifier`: "Can be either be its id
//!   or current name"), so the form matters as much as the id's does.
//! * A template's `MetricName` is a metric the service named by its
//!   `TargetResourceType` publishes to CloudWatch, which is what
//!   `MetricName` requires ("Must be compatible with targetResourceType").
//! * The ids are the CATALOG's: a published group is published under the id the
//!   service assigns it, in the `aws-` form the `Id` member documents, and the
//!   ids below are the ones the recordings of this catalog disclose (see
//!   [`PublishedGroup::id`]). A published TEMPLATE's id is still assigned here,
//!   in catalog order, because no recording discloses which published template
//!   each of the service's template ids names — so an id names the same
//!   published template on every call and in every process.
//! * The alarm settings (period, evaluation periods, datapoints, threshold,
//!   missing-data treatment) and the prose of a template's `Description` are
//!   likewise CHOSEN, as the [`super::offerings`] catalog's prices are: no AWS
//!   document gives them machine-readably. Each is a pure function of the row, so
//!   two reads of one published template report the same settings, but nothing
//!   should read them as AWS's published alarm thresholds.
//! * `CreatedAt`/`ModifiedAt` are the ONE published instant
//!   ([`CATALOG_PUBLISHED_AT`]) rather than the instant the store came up: a
//!   published catalog's timestamps are the same on every run, so what a caller
//!   caches about one entry stays true.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use super::{CloudWatchAlarmTemplateGroupRecord, CloudWatchAlarmTemplateRecord};

/// The instant every published entry of this catalog reports as its `CreatedAt`
/// and its `ModifiedAt`: seconds since the Unix epoch at 2024-01-01T00:00:00Z.
///
/// FIXED, and deliberately not the instant the account's store came up: these
/// records are not created by anything this process does, so a value taken from
/// the local clock would make one published template report a different
/// `CreatedAt` on every run. The particular instant is this emulator's own — no
/// recording in this corpus discloses when AWS published the catalog — and it is
/// in the past of any run, which is what `CreatedAt` means for an entry a caller
/// has always been able to read.
pub const CATALOG_PUBLISHED_AT: u64 = 1_704_067_200;

/// [`CATALOG_PUBLISHED_AT`] as an instant.
fn published_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(CATALOG_PUBLISHED_AT)
}

/// The ARN of a published monitor template resource in `region`.
///
/// A published resource belongs to the SERVICE rather than to an account, so the
/// ARN's account-id field is EMPTY — which is what tells a client that the
/// resource it names is AWS-provided and not one of its own — while the region
/// field names the regional endpoint the resource is read through. An
/// account-owned resource of the same family keeps the account-bearing form
/// `::engine::emulator_arn` builds.
pub fn service_owned_arn(region: &str, resource: &str) -> String {
    format!("arn:aws:medialive:{region}::{resource}")
}

/// The prefix every published id carries, as the `Id` members document it
/// ("AWS provided template groups have ids that start with `aws-`").
const PUBLISHED_ID_PREFIX: &str = super::template_scope::AWS_PROVIDED_ID_PREFIX;

/// The id of the published template at `position` in the catalog's template
/// sequence, counting from one — in the digit range of its own that keeps the
/// group and template id spaces apart, as the account's own mints do.
fn template_id(position: usize) -> String {
    format!("{PUBLISHED_ID_PREFIX}{:07}", 2_000_000 + position)
}

/// Whether `id` names a resource of this published catalog rather than one the
/// account created.
pub fn is_published_id(id: &str) -> bool {
    id.starts_with(PUBLISHED_ID_PREFIX)
}

/// The place the monitor template resource `id` takes in a listing of its kind:
/// the resources the SERVICE published before the account existed first, then the
/// ones the account created, each in id order — the order the catalog's own ids
/// sort in for the published ones, and mint order for the account's.
///
/// A listing reports the resources of a kind in the order they came into being,
/// and every entry of this catalog was published before any account resource was
/// created ([`CATALOG_PUBLISHED_AT`]) — so a page cut at a given size reports the
/// published catalog before anything the caller made. The key is derived from the
/// ID ALONE, which is what a `NextToken` carries, so a token whose resource was
/// deleted since the previous page still names the place to resume at.
pub fn listing_position(id: &str) -> (u8, &str) {
    (u8::from(!is_published_id(id)), id)
}

/// Why a write to a monitor template resource changed nothing.
///
/// The two refusals every write of this family shares: nothing in the request's
/// region answers to the identifier, or the resource it names is one the SERVICE
/// publishes — which this account reads like any other and may not write.
#[derive(Debug)]
pub enum PublishedWriteRefusal {
    /// No resource of that type in the request's region answers to the
    /// identifier, by either form `Identifier` documents (its id or its current
    /// name).
    NotFound,
    /// The identifier names a resource of the published catalog.
    Published,
}

/// The message a write refused because its subject is one the service publishes
/// carries: the resource type, the identifier the request named, then that it is
/// AWS-provided and not the account's to change.
///
/// ONE builder, so no two of the operations that write a monitor template
/// resource word that refusal two ways; the identifier comes from the request,
/// so the message says what the caller must change. The FIXED text is this
/// emulator's own and UNVERIFIED against the real service: no recording of a
/// write to an AWS-provided monitor template resource is available to take the
/// sentence from, so it states the rule plainly rather than guessing at a
/// wording.
pub fn published_resource_message(resource_type: &str, identifier: &str) -> String {
    format!("{resource_type} {identifier} is provided by AWS and cannot be modified")
}

/// One published alarm template, before it becomes a
/// [`CloudWatchAlarmTemplateRecord`].
#[derive(Debug, Clone, Copy)]
pub struct PublishedTemplate {
    /// The `CloudWatchAlarmTemplateTargetResourceType` the template generates
    /// alarms for, spelled as the model declares it.
    pub target_resource_type: &'static str,
    /// A metric the service that owns [`Self::target_resource_type`] publishes
    /// to CloudWatch.
    pub metric_name: &'static str,
    /// The `CloudWatchAlarmTemplateComparisonOperator`.
    pub comparison_operator: &'static str,
    /// The `CloudWatchAlarmTemplateStatistic`.
    pub statistic: &'static str,
    /// `Period`, in seconds, inside `__integerMin10Max86400`.
    pub period: i32,
    /// `EvaluationPeriods`, the N of the alarm's "M out of N".
    pub evaluation_periods: i32,
    /// `DatapointsToAlarm`, its M.
    pub datapoints_to_alarm: i32,
    pub threshold: f64,
    /// The `CloudWatchAlarmTemplateTreatMissingData`.
    pub treat_missing_data: &'static str,
}

/// One published template group, before it becomes a
/// [`CloudWatchAlarmTemplateGroupRecord`].
#[derive(Debug, Clone, Copy)]
pub struct PublishedGroup {
    /// The `Id` the service publishes the group under, in the `aws-` form the
    /// `Id` member documents for a published resource ("AWS provided template
    /// groups have ids that start with `aws-`") — the prefix and seven digits,
    /// which is all `__stringMin7Max11PatternAws097` admits.
    ///
    /// The id is the CATALOG's, not a counter of this emulator's: a client pins
    /// a published group by it (a signal map association, a template created
    /// inside a published group, infrastructure that records the ARN built from
    /// it), so it has to be the id that names this group on the service, and it
    /// is therefore the same in every region, for every account and in every
    /// process. The pairs below are the published id of each published group, as
    /// the recordings of this catalog report them — the service assigns the
    /// digits, and nothing derives them from the group's position or name.
    pub id: &'static str,
    /// The group's `Name`, as the service publishes it.
    pub name: &'static str,
    pub templates: &'static [PublishedTemplate],
}

/// The prefix every published monitor template resource's `Name` carries, the
/// one the published group names themselves are published under.
const PUBLISHED_NAME_PREFIX: &str = "AWS-";

/// The workflow family a published group covers, as a published name spells it:
/// the group's own `Name` without the prefix every published name carries
/// (`AWS-MediaLiveUdpAndRtmpWorkflows` -> `MediaLiveUdpAndRtmpWorkflows`).
fn workflow_family(group_name: &str) -> &str {
    group_name
        .strip_prefix(PUBLISHED_NAME_PREFIX)
        .unwrap_or(group_name)
}

/// Which side of the threshold a breach of an alarm built from this template
/// lies on, as a published template's name spells it: a comparison that trips
/// when the statistic rises above the threshold alarms on a HIGH metric, and one
/// that trips when it falls under the threshold on a LOW one.
fn breach_direction(comparison_operator: &str) -> &'static str {
    match comparison_operator {
        comparison_operator::LESS_THAN | comparison_operator::LESS_THAN_OR_EQUAL_TO => "Low",
        // `GreaterThanThreshold`, `GreaterThanOrEqualToThreshold`, and anything
        // a later model adds.
        _ => "High",
    }
}

impl PublishedTemplate {
    /// The name the published template carries: the metric the alarm watches and
    /// the direction a breach of it lies in, then the workflow family of the
    /// group that publishes it, under the prefix every published monitor
    /// resource's name carries.
    ///
    /// DERIVED from the row and its group, so no two rows of this table can
    /// collide on a name and a published name cannot drift from the alarm it
    /// names. Both components are the catalog's own vocabulary: the metric is
    /// the one the alarm is evaluated on and the family is the publishing
    /// group's `Name`, which is what keeps every published template's `Name`
    /// unique in the region (`Name`: "Names must be unique within the scope of a
    /// resource type in a specific region").
    fn name(&self, group_name: &str) -> String {
        format!(
            "{PUBLISHED_NAME_PREFIX}{metric}{direction}-{family}",
            metric = self.metric_name,
            direction = breach_direction(self.comparison_operator),
            family = workflow_family(group_name),
        )
    }

    /// The sentence this template's `Description` carries: what the alarm trips
    /// on, read out of the row's own settings.
    ///
    /// The emulator's own prose (see the module docs), derived rather than
    /// stored, so it cannot drift from the settings beside it.
    fn description(&self) -> String {
        format!(
            "Alarms when the {statistic} of {metric} is {sense} {threshold} \
             for {datapoints} of {evaluation_periods} periods of {period} seconds.",
            statistic = self.statistic,
            metric = self.metric_name,
            sense = comparison_sense(self.comparison_operator),
            threshold = self.threshold,
            datapoints = self.datapoints_to_alarm,
            evaluation_periods = self.evaluation_periods,
            period = self.period,
        )
    }
}

/// How a comparison operator reads in a description: which side of the threshold
/// the breaching statistic is on, and whether the threshold itself breaches.
fn comparison_sense(comparison_operator: &str) -> &'static str {
    match comparison_operator {
        comparison_operator::GREATER_THAN_OR_EQUAL_TO => "at or above",
        comparison_operator::LESS_THAN => "below",
        comparison_operator::LESS_THAN_OR_EQUAL_TO => "at or below",
        // `GreaterThanThreshold`, and anything a later model adds.
        _ => "above",
    }
}

/// The `CloudWatchAlarmTemplateComparisonOperator` values this catalog names,
/// spelled exactly as the model declares them (`smithy_query(query="shape",
/// shape="CloudWatchAlarmTemplateComparisonOperator")`).
pub mod comparison_operator {
    pub const GREATER_THAN: &str = "GreaterThanThreshold";
    pub const GREATER_THAN_OR_EQUAL_TO: &str = "GreaterThanOrEqualToThreshold";
    pub const LESS_THAN: &str = "LessThanThreshold";
    pub const LESS_THAN_OR_EQUAL_TO: &str = "LessThanOrEqualToThreshold";
}

/// The `CloudWatchAlarmTemplateStatistic` values this catalog names
/// (`smithy_query(query="shape", shape="CloudWatchAlarmTemplateStatistic")`).
pub mod statistic {
    pub const AVERAGE: &str = "Average";
    pub const MAXIMUM: &str = "Maximum";
    pub const MINIMUM: &str = "Minimum";
    pub const SUM: &str = "Sum";
}

/// The `CloudWatchAlarmTemplateTreatMissingData` values this catalog names
/// (`smithy_query(query="shape",
/// shape="CloudWatchAlarmTemplateTreatMissingData")`).
pub mod treat_missing_data {
    pub const BREACHING: &str = "breaching";
    pub const MISSING: &str = "missing";
    pub const NOT_BREACHING: &str = "notBreaching";
}

/// The `CloudWatchAlarmTemplateTargetResourceType` values this catalog names
/// (`smithy_query(query="shape",
/// shape="CloudWatchAlarmTemplateTargetResourceType")`).
pub mod target_resource_type {
    pub const CLOUDFRONT_DISTRIBUTION: &str = "CLOUDFRONT_DISTRIBUTION";
    pub const MEDIACONNECT_FLOW: &str = "MEDIACONNECT_FLOW";
    pub const MEDIALIVE_CHANNEL: &str = "MEDIALIVE_CHANNEL";
    pub const MEDIALIVE_MULTIPLEX: &str = "MEDIALIVE_MULTIPLEX";
    pub const MEDIAPACKAGE_CHANNEL: &str = "MEDIAPACKAGE_CHANNEL";
    pub const MEDIAPACKAGE_ORIGIN_ENDPOINT: &str = "MEDIAPACKAGE_ORIGIN_ENDPOINT";
    pub const MEDIATAILOR_PLAYBACK_CONFIGURATION: &str = "MEDIATAILOR_PLAYBACK_CONFIGURATION";
}

/// The published catalog, written in the order the groups' names sort in.
///
/// The TABLE's order is the table's own: a listing reports the published
/// resources of a kind in id order ([`listing_position`]), which is the order a
/// `NextToken` resumes a page at, so where a row sits here changes nothing a
/// caller sees.
///
/// The set is the same in every region and for every account: this is what the
/// service publishes, not what a caller created.
pub const CATALOG: &[PublishedGroup] = &[
    PublishedGroup {
        id: "aws-1160891",
        name: "AWS-CloudFrontGeneralWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::CLOUDFRONT_DISTRIBUTION,
                metric_name: "4xxErrorRate",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 5.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::CLOUDFRONT_DISTRIBUTION,
                metric_name: "5xxErrorRate",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-4336130",
        name: "AWS-MediaConnectArqProtocolWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceNotRecoveredPackets",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceRoundTripTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 200.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceDisconnects",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 1,
                datapoints_to_alarm: 1,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-2587288",
        name: "AWS-MediaConnectCdiJpegxsWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourcePacketLossPercent",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.01,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceConnected",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::MINIMUM,
                period: 60,
                evaluation_periods: 2,
                datapoints_to_alarm: 2,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceBitRate",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::AVERAGE,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-5128237",
        name: "AWS-MediaConnectInputFailoverWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "FailoverSwitches",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 300,
                evaluation_periods: 1,
                datapoints_to_alarm: 1,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceConnected",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::MINIMUM,
                period: 60,
                evaluation_periods: 2,
                datapoints_to_alarm: 2,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-3414886",
        name: "AWS-MediaConnectRtpWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceNotRecoveredPackets",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceTSSyncLoss",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourceContinuityCounter",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIACONNECT_FLOW,
                metric_name: "SourcePacketLossPercent",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.01,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-4237617",
        name: "AWS-MediaLiveMultiplexWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_MULTIPLEX,
                metric_name: "ActiveAlerts",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::MAXIMUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "ChannelInputErrorSeconds",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-5775786",
        name: "AWS-MediaLiveToMediaPackageWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "Output4xxErrors",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "Output5xxErrors",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIAPACKAGE_CHANNEL,
                metric_name: "IngressResponseTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1000.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIAPACKAGE_ORIGIN_ENDPOINT,
                metric_name: "EgressResponseTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1000.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "InputLossSeconds",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 1,
                datapoints_to_alarm: 1,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-1484347",
        name: "AWS-MediaLiveUdpAndRtmpWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "ActiveAlerts",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::MAXIMUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "InputVideoFrameRate",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::AVERAGE,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 20.0,
                treat_missing_data: treat_missing_data::BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "FillMsec",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::MAXIMUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1000.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "DroppedFrames",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            // The one published template of this group a recording of the
            // catalog discloses the settings of, carried as it reports them.
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "ChannelInputErrorSeconds",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::MAXIMUM,
                period: 60,
                evaluation_periods: 1,
                datapoints_to_alarm: 1,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::MISSING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "InputLossSeconds",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 60,
                evaluation_periods: 1,
                datapoints_to_alarm: 1,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "PrimaryInputActive",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::MINIMUM,
                period: 60,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "SvqTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIALIVE_CHANNEL,
                metric_name: "NetworkOut",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-2520023",
        name: "AWS-MediaPackageGeneralWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIAPACKAGE_CHANNEL,
                metric_name: "IngressResponseTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1000.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIAPACKAGE_ORIGIN_ENDPOINT,
                metric_name: "EgressRequestCount",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::SUM,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 3,
                threshold: 1.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIAPACKAGE_ORIGIN_ENDPOINT,
                metric_name: "EgressResponseTime",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 1000.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
    PublishedGroup {
        id: "aws-3840949",
        name: "AWS-MediaTailorAdInsertionWorkflows",
        templates: &[
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIATAILOR_PLAYBACK_CONFIGURATION,
                metric_name: "AdDecisionServer.Errors",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIATAILOR_PLAYBACK_CONFIGURATION,
                metric_name: "AdDecisionServer.Timeouts",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIATAILOR_PLAYBACK_CONFIGURATION,
                metric_name: "Avail.FillRate",
                comparison_operator: comparison_operator::LESS_THAN,
                statistic: statistic::AVERAGE,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.9,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
            PublishedTemplate {
                target_resource_type: target_resource_type::MEDIATAILOR_PLAYBACK_CONFIGURATION,
                metric_name: "Origin.Errors",
                comparison_operator: comparison_operator::GREATER_THAN,
                statistic: statistic::SUM,
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: 2,
                threshold: 0.0,
                treat_missing_data: treat_missing_data::NOT_BREACHING,
            },
        ],
    },
];

/// The published groups of `region`, as the records a region's partition is
/// seeded with: one per row of [`CATALOG`], in catalog order.
///
/// `Description` is absent: the catalog publishes a group under its name, and
/// nothing derives a description for one.
pub fn published_groups(region: &str) -> Vec<CloudWatchAlarmTemplateGroupRecord> {
    CATALOG
        .iter()
        .map(|group| {
            let id = group.id.to_string();
            CloudWatchAlarmTemplateGroupRecord {
                arn: service_owned_arn(region, &format!("cloudwatch-alarm-template-group:{id}")),
                id,
                region: region.to_string(),
                name: group.name.to_string(),
                description: None,
                tags: HashMap::new(),
                created_at: published_at(),
                modified_at: published_at(),
            }
        })
        .collect()
}

/// The published templates of `region`, as the records a region's partition is
/// seeded with: the templates of each catalog group, in catalog order, each
/// carrying the id of the group that publishes it.
pub fn published_templates(region: &str) -> Vec<CloudWatchAlarmTemplateRecord> {
    let mut templates = Vec::new();
    for group in CATALOG {
        for template in group.templates {
            let id = template_id(templates.len() + 1);
            templates.push(CloudWatchAlarmTemplateRecord {
                arn: service_owned_arn(region, &format!("cloudwatch-alarm-template:{id}")),
                id,
                region: region.to_string(),
                group_id: group.id.to_string(),
                name: template.name(group.name),
                description: Some(template.description()),
                comparison_operator: template.comparison_operator.to_string(),
                datapoints_to_alarm: Some(template.datapoints_to_alarm),
                evaluation_periods: template.evaluation_periods,
                metric_name: template.metric_name.to_string(),
                period: template.period,
                statistic: template.statistic.to_string(),
                target_resource_type: template.target_resource_type.to_string(),
                threshold: template.threshold,
                treat_missing_data: template.treat_missing_data.to_string(),
                tags: HashMap::new(),
                created_at: published_at(),
                modified_at: published_at(),
            });
        }
    }
    templates
}
