// SPDX-License-Identifier: CC0-1.0

//! The TELEMETRY CONFIGURATIONS of the account's own resources — what the telemetry-config
//! audit (`ListResourceTelemetry`) reports.
//!
//! A telemetry configuration is not a resource of this service. It is what telemetry ONE
//! resource of another service has configured — "a list of telemetry configurations for
//! Amazon Web Services resources supported by telemetry config in the caller's account"
//! (`ListResourceTelemetryOutput$TelemetryConfigurations`) — so every member of it is READ
//! where the service that OWNS the resource holds it, over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), the way an emulated service
//! reaches another. This crate therefore names no other service's crate, nothing leaves the
//! process, and a resource this emulator does not hold is reported for by nothing.
//!
//! Two consequences this module is built around:
//!
//! * Nothing is CACHED and nothing is written: a resource created a moment ago is audited at
//!   once, one that was deleted stops being audited at once, and an audit changes nothing a
//!   later caller sees.
//! * A read the owning service could not answer IN FULL is evidence of nothing. Reporting
//!   such a resource type as empty would say the account holds none of them, so that type is
//!   left out of the audit entirely and the gap is logged, rather than reported as an
//!   account with nothing in it.
//!
//! ## Which resource types an audit discovers
//!
//! A telemetry configuration carries the STATE of each kind of telemetry for the resource
//! (`TelemetryConfiguration$TelemetryConfigurationState`, a `TelemetryType` ->
//! `TelemetryState` map), so a resource can only be audited where that state can be read
//! from the owning service. [`DISCOVERED_RESOURCE_TYPES`] is therefore the resource types
//! whose telemetry configuration the owning service reports in a REGION-WIDE listing — one
//! listing per kind of configuration, never a read per resource, so the work one audit does
//! is bounded by the pages it reads rather than by how many resources the account holds:
//!
//! | resource type | telemetry | read from |
//! |---|---|---|
//! | `AWS::EC2::Instance` | `Metrics` | `DescribeInstances`' `Instance$Monitoring$State` — the instance's detailed monitoring |
//! | `AWS::EC2::VPC` | `Logs` | `DescribeFlowLogs`' flow logs for the VPC, whose source type is `VPC_FLOW_LOGS` |
//!
//! Every other `TelemetryType` of an audited resource is `NotApplicable`: that resource has
//! no configuration for it, which is what that value of the enum says. A supported resource
//! type absent from the table is one whose telemetry configuration no region-wide listing of
//! its owner reports (an EKS cluster's log types, a web ACL's logging configuration and a
//! load balancer's access-log attributes each take a read per resource), so an audit reports
//! no configuration for it rather than one nothing was read for.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ops::types::TelemetryConfiguration;

/// `AWS::EC2::Instance`, a `ResourceType` value
/// (`crate::ops::types::RESOURCE_TYPE_VALUES`).
pub const EC2_INSTANCE_RESOURCE_TYPE: &str = "AWS::EC2::Instance";

/// `AWS::EC2::VPC`, a `ResourceType` value.
pub const EC2_VPC_RESOURCE_TYPE: &str = "AWS::EC2::VPC";

/// The resource types an audit DISCOVERS, in the order a request that narrows to none of
/// them reports them — so a listing has one stable sequence to page over.
///
/// The table in this module's docs states where each one's telemetry configuration is read
/// from, and why a supported resource type is absent from it.
pub const DISCOVERED_RESOURCE_TYPES: &[&str] = &[EC2_INSTANCE_RESOURCE_TYPE, EC2_VPC_RESOURCE_TYPE];

/// `TelemetryState$Enabled`: the telemetry is configured for the resource.
pub const TELEMETRY_STATE_ENABLED: &str = "Enabled";

/// `TelemetryState$Disabled`: the telemetry applies to the resource and is not configured.
pub const TELEMETRY_STATE_DISABLED: &str = "Disabled";

/// `TelemetryState$NotApplicable`: this kind of telemetry is not something the resource has
/// a configuration for at all.
pub const TELEMETRY_STATE_NOT_APPLICABLE: &str = "NotApplicable";

/// `TelemetryType$Logs`.
pub const TELEMETRY_TYPE_LOGS: &str = "Logs";

/// `TelemetryType$Metrics`.
pub const TELEMETRY_TYPE_METRICS: &str = "Metrics";

/// How many pages of ONE owning-service listing an audit reads before it gives up on that
/// listing.
///
/// A HOST bound on the work one audit does, not a published quota: the listing is followed
/// by its own token, and without a ceiling one audit would follow it without end. A listing
/// that is not read to its end is answered as an unread listing (see the module docs), never
/// as a complete one, so the ceiling can only leave a resource unaudited — it never invents
/// one or reports one as gone.
const MAX_DISCOVERY_PAGES: usize = 20;

/// The `Monitoring$State` values in which EC2 reports DETAILED monitoring as being in force
/// for an instance: `enabled`, and the transient `disabling` it passes through on its way
/// out, during which the instance is still monitored. The other two values — `disabled` and
/// the transient `pending` on the way in — are the states in which it is not.
const DETAILED_MONITORING_STATES: [&str; 2] = ["enabled", "disabling"];

/// The `FlowLog$FlowLogStatus` of a flow log that is delivering: "the status of the flow log
/// (`ACTIVE`)".
const FLOW_LOG_STATUS_ACTIVE: &str = "ACTIVE";

/// The telemetry configuration of ONE resource, as the owning service reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceTelemetry {
    /// `TelemetryConfiguration$ResourceType`: which supported type the resource is of.
    pub resource_type: String,
    /// `TelemetryConfiguration$ResourceIdentifier`: the identifier the owning service holds
    /// the resource under, which is what `ResourceIdentifierPrefix` narrows a listing by.
    pub resource_identifier: String,
    /// `TelemetryConfiguration$TelemetryConfigurationState`: the state of EVERY modelled
    /// `TelemetryType` for this resource, so a listing can be narrowed by any of them.
    pub telemetry_configuration_state: BTreeMap<String, String>,
    /// `TelemetryConfiguration$TelemetrySourceType`: the telemetry source the resource's
    /// type correlates with, when the model correlates one with it.
    pub telemetry_source_type: Option<String>,
    /// `TelemetryConfiguration$ResourceTags`: the tags the owning service reports on the
    /// resource, empty when it reports none.
    pub resource_tags: BTreeMap<String, String>,
    /// `TelemetryConfiguration$LastUpdateTimeStamp`: when the configuration this state was
    /// read from was last set, in epoch milliseconds — absent when the owning service
    /// reports no instant for it, rather than filled in with the time of the audit.
    pub last_update_timestamp_ms: Option<i64>,
}

impl ResourceTelemetry {
    /// This record as the modelled shape a listing reports it in.
    ///
    /// `AccountIdentifier` is the emulator's own account: the audit covers "the caller's
    /// account", and this emulator serves exactly one.
    pub fn configuration(&self) -> TelemetryConfiguration {
        TelemetryConfiguration {
            account_identifier: Some(::engine::emulator_account_id().to_string()),
            telemetry_configuration_state: Some(
                self.telemetry_configuration_state
                    .iter()
                    .map(|(telemetry_type, state)| (telemetry_type.clone(), state.clone()))
                    .collect(),
            ),
            resource_type: Some(self.resource_type.clone()),
            resource_identifier: Some(self.resource_identifier.clone()),
            resource_tags: Some(
                self.resource_tags
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            last_update_time_stamp: self.last_update_timestamp_ms,
            telemetry_source_type: self.telemetry_source_type.clone(),
        }
    }

    /// Whether this resource carries every tag `wanted` names with that tag's value —
    /// `ResourceTags` is "a key-value pair to filter resources based on tags associated with
    /// the resource", so a resource has to carry all of the pairs to be reported.
    pub fn carries_tags(&self, wanted: &HashMap<String, String>) -> bool {
        wanted
            .iter()
            .all(|(key, value)| self.resource_tags.get(key) == Some(value))
    }

    /// Whether this resource's telemetry is in every state `wanted` names — the companion
    /// of [`Self::carries_tags`] for `TelemetryConfigurationState`, "the telemetry type and
    /// the state of the telemetry configuration".
    pub fn carries_telemetry_states(&self, wanted: &HashMap<String, String>) -> bool {
        wanted.iter().all(|(telemetry_type, state)| {
            self.telemetry_configuration_state.get(telemetry_type) == Some(state)
        })
    }
}

/// The telemetry configurations of this account's resources in `region`, for the resource
/// types `requested` names — or, when it names none, for every type an audit discovers.
///
/// Ordered by resource type and then by resource identifier, so repeated audits of unchanged
/// resources report them in the same sequence and a listing can page over it. A resource
/// type whose owning service could not be read is left out (see the module docs).
///
/// Call it with NO guard of this service's store held: it awaits.
pub async fn discovered(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    requested: &BTreeSet<String>,
) -> Vec<ResourceTelemetry> {
    let mut audited: Vec<ResourceTelemetry> = Vec::new();
    for resource_type in DISCOVERED_RESOURCE_TYPES {
        if !requested.is_empty() && !requested.contains(*resource_type) {
            continue;
        }
        let read = match *resource_type {
            EC2_INSTANCE_RESOURCE_TYPE => instance_telemetry(ctx, region).await,
            EC2_VPC_RESOURCE_TYPE => vpc_telemetry(ctx, region).await,
            // `DISCOVERED_RESOURCE_TYPES` and this match are the two halves of one table:
            // a type listed there with no read here is audited by nothing.
            _ => None,
        };
        match read {
            Some(records) => audited.extend(records),
            None => ::tracing::warn!(
                resource_type = *resource_type,
                region,
                "the telemetry-config audit could not read this resource type's owning \
                 service, so it reports no configuration for it"
            ),
        }
    }
    audited.sort_by(|left, right| {
        type_rank(&left.resource_type)
            .cmp(&type_rank(&right.resource_type))
            .then_with(|| left.resource_identifier.cmp(&right.resource_identifier))
    });
    audited
}

/// Where `resource_type` sits in [`DISCOVERED_RESOURCE_TYPES`], which is the order an audit
/// that was narrowed to no type reports the types in.
pub fn type_rank(resource_type: &str) -> usize {
    DISCOVERED_RESOURCE_TYPES
        .iter()
        .position(|discovered| *discovered == resource_type)
        .unwrap_or(DISCOVERED_RESOURCE_TYPES.len())
}

/// The telemetry configuration of every EC2 instance this account holds in `region`, or
/// `None` when EC2's instance listing could not be read in full.
///
/// The audited telemetry is the instance's METRICS configuration: EC2 publishes an
/// instance's metrics at the cadence its monitoring state selects, and
/// `Instance$Monitoring$State` is where that state is reported, so the configuration's
/// `Metrics` is `Enabled` exactly while EC2 reports detailed monitoring in force
/// ([`DETAILED_MONITORING_STATES`]) and `Disabled` otherwise.
///
/// A TERMINATED instance no longer exists ("the instance has been terminated",
/// `InstanceStateName`), so it is not a resource of the account and nothing is audited for
/// it.
async fn instance_telemetry(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<Vec<ResourceTelemetry>> {
    let client = ec2(ctx, region)?;

    let mut reservations: Vec<::aws_sdk_ec2::types::Reservation> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut read_in_full = false;
    for _ in 0..MAX_DISCOVERY_PAGES {
        let page = client
            .describe_instances()
            .set_next_token(next_token.clone())
            .send()
            .await
            .inspect_err(|error| {
                ::tracing::warn!(
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
                    "the telemetry-config audit could not read this account's EC2 instances"
                );
            })
            .ok()?;
        reservations.extend(page.reservations.unwrap_or_default());
        next_token = page.next_token;
        if next_token.is_none() {
            read_in_full = true;
            break;
        }
    }
    if !read_in_full {
        return None;
    }

    let mut audited = Vec::new();
    for reservation in &reservations {
        for instance in reservation.instances() {
            let Some(instance_id) = instance
                .instance_id
                .clone()
                .filter(|instance_id| !instance_id.is_empty())
            else {
                continue;
            };
            if instance
                .state
                .as_ref()
                .and_then(|state| state.name.as_ref())
                .is_some_and(|name| name == &::aws_sdk_ec2::types::InstanceStateName::Terminated)
            {
                continue;
            }
            let monitored = instance
                .monitoring
                .as_ref()
                .and_then(|monitoring| monitoring.state.as_ref())
                .is_some_and(|state| DETAILED_MONITORING_STATES.contains(&state.as_str()));

            audited.push(ResourceTelemetry {
                resource_type: EC2_INSTANCE_RESOURCE_TYPE.to_string(),
                resource_identifier: instance_id,
                telemetry_configuration_state: telemetry_state(
                    TELEMETRY_TYPE_METRICS,
                    monitored,
                ),
                telemetry_source_type: correlated_source_type(EC2_INSTANCE_RESOURCE_TYPE),
                resource_tags: reported_tags(instance.tags()),
                // EC2 reports no instant at which an instance's monitoring state was last
                // set, so the configuration carries none.
                last_update_timestamp_ms: None,
            });
        }
    }
    Some(audited)
}

/// The telemetry configuration of every VPC this account holds in `region`, or `None` when
/// EC2's VPC listing or its flow-log listing could not be read in full.
///
/// The audited telemetry is the VPC's LOGS configuration: a VPC's logs are its flow logs, so
/// `Logs` is `Enabled` exactly while EC2 reports a delivering flow log that monitors the VPC
/// and `Disabled` otherwise, and the configuration's `LastUpdateTimeStamp` is when the
/// newest of those flow logs was created (`FlowLog$CreationTime`).
async fn vpc_telemetry(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<Vec<ResourceTelemetry>> {
    let client = ec2(ctx, region)?;
    let flow_logs = active_flow_logs(&client).await?;

    let mut vpcs: Vec<::aws_sdk_ec2::types::Vpc> = Vec::new();
    let mut next_token: Option<String> = None;
    let mut read_in_full = false;
    for _ in 0..MAX_DISCOVERY_PAGES {
        let page = client
            .describe_vpcs()
            .set_next_token(next_token.clone())
            .send()
            .await
            .inspect_err(|error| {
                ::tracing::warn!(
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
                    "the telemetry-config audit could not read this account's VPCs"
                );
            })
            .ok()?;
        vpcs.extend(page.vpcs.unwrap_or_default());
        next_token = page.next_token;
        if next_token.is_none() {
            read_in_full = true;
            break;
        }
    }
    if !read_in_full {
        return None;
    }

    Some(
        vpcs.iter()
            .filter_map(|vpc| {
                let vpc_id = vpc.vpc_id.clone().filter(|vpc_id| !vpc_id.is_empty())?;
                let logged = flow_logs.get(&vpc_id);
                Some(ResourceTelemetry {
                    resource_type: EC2_VPC_RESOURCE_TYPE.to_string(),
                    resource_identifier: vpc_id,
                    telemetry_configuration_state: telemetry_state(
                        TELEMETRY_TYPE_LOGS,
                        logged.is_some(),
                    ),
                    telemetry_source_type: correlated_source_type(EC2_VPC_RESOURCE_TYPE),
                    resource_tags: reported_tags(vpc.tags()),
                    last_update_timestamp_ms: logged.copied().flatten(),
                })
            })
            .collect(),
    )
}

/// Which resources EC2 reports a DELIVERING flow log for, each with the epoch-millisecond
/// instant the newest of its flow logs was created at — or `None` when the flow-log listing
/// could not be read in full.
///
/// Keyed by the monitored resource (`FlowLog$ResourceId`), so one lookup answers whether a
/// VPC has its logs configured however many flow logs the account holds.
async fn active_flow_logs(
    client: &::aws_sdk_ec2::Client,
) -> Option<BTreeMap<String, Option<i64>>> {
    let mut active: BTreeMap<String, Option<i64>> = BTreeMap::new();
    let mut next_token: Option<String> = None;
    for _ in 0..MAX_DISCOVERY_PAGES {
        let page = client
            .describe_flow_logs()
            .set_next_token(next_token.clone())
            .send()
            .await
            .inspect_err(|error| {
                ::tracing::warn!(
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
                    "the telemetry-config audit could not read this account's flow logs"
                );
            })
            .ok()?;
        for flow_log in page.flow_logs() {
            if flow_log
                .flow_log_status
                .as_deref()
                .is_some_and(|status| status != FLOW_LOG_STATUS_ACTIVE)
            {
                continue;
            }
            let Some(resource_id) = flow_log
                .resource_id
                .clone()
                .filter(|resource_id| !resource_id.is_empty())
            else {
                continue;
            };
            let created_at = flow_log
                .creation_time
                .as_ref()
                .and_then(|at| at.to_millis().ok());
            let newest = active.entry(resource_id).or_default();
            if created_at > *newest {
                *newest = created_at;
            }
        }
        next_token = page.next_token;
        if next_token.is_none() {
            return Some(active);
        }
    }
    None
}

/// The `TelemetryConfigurationState` of a resource whose ONE audited kind of telemetry is
/// `audited`, in the state `configured` says it is in.
///
/// Every other modelled `TelemetryType` is `NotApplicable`: the resource has no
/// configuration of that kind for the audit to report a state of. The map therefore carries
/// an entry per `TelemetryType` the model declares, which is what lets a listing be narrowed
/// by any of them.
fn telemetry_state(audited: &str, configured: bool) -> BTreeMap<String, String> {
    crate::ops::types::TELEMETRY_TYPE_VALUES
        .iter()
        .map(|telemetry_type| {
            let state = if *telemetry_type != audited {
                TELEMETRY_STATE_NOT_APPLICABLE
            } else if configured {
                TELEMETRY_STATE_ENABLED
            } else {
                TELEMETRY_STATE_DISABLED
            };
            ((*telemetry_type).to_string(), state.to_string())
        })
        .collect()
}

/// The `TelemetrySourceType` a resource of `resource_type` carries: the source type the
/// model correlates with that resource type, read through this crate's ONE statement of that
/// correlation ([`crate::state::correlated_telemetry_source_types`]) so an audit and a
/// telemetry rule never disagree about which source a resource's logs come from.
fn correlated_source_type(resource_type: &str) -> Option<String> {
    crate::state::correlated_telemetry_source_types(Some(resource_type), TELEMETRY_TYPE_LOGS)?
        .into_iter()
        .next()
}

/// The tags an owning service reported on a resource, as the audit reports them: a tag with
/// no key names nothing, and a tag with no value carries the empty value AWS stores for it.
fn reported_tags(tags: &[::aws_sdk_ec2::types::Tag]) -> BTreeMap<String, String> {
    tags.iter()
        .filter_map(|tag| {
            Some((
                tag.key.clone().filter(|key| !key.is_empty())?,
                tag.value.clone().unwrap_or_default(),
            ))
        })
        .collect()
}

/// An Amazon EC2 client for `region` over the in-process callback.
///
/// The endpoint URL is only the address that transport routes on IN PROCESS: no socket is
/// opened, no name is resolved and nothing leaves this process. The credentials are the
/// callback's, which sign this in-process hop and never appear in a response.
///
/// The Region a request was signed for is caller-supplied text and here it would end up in
/// an endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, this tree's one statement of that grammar) instead
/// of being interpolated as given: `None` for a value that would name an authority the
/// caller chose rather than the peer service this emulator asks.
fn ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let Some(region) = ::service_common::arn::region_label(region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return None;
    };
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .http_client(ctx.callback().http_client())
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}
