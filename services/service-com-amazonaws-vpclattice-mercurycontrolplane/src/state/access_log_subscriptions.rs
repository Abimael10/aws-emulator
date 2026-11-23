// SPDX-License-Identifier: CC0-1.0

//! The ACCESS LOG SUBSCRIPTION: what one holds, which resource of this service it monitors,
//! and what its DESTINATION has to be for the subscription to deliver anything.
//!
//! "Enables access logs to be sent to Amazon CloudWatch, Amazon S3, and Amazon Kinesis Data
//! Firehose" (`CreateAccessLogSubscription`'s own documentation). A subscription is a
//! RESOURCE of its own — it has an id, an ARN, tags and two instants — so it lives in the one
//! store beside the service networks and services it monitors, and every operation that
//! reports one reads it from here.
//!
//! Two readings live here beside the record, because both are the same question asked of a
//! caller's identifier:
//!
//! * which resource of THIS service a `resourceIdentifier` names ([`subscribed_resource`]) —
//!   the member admits a service network, a service or a resource configuration, in the bare
//!   id or the ARN form;
//! * which resource of ANOTHER service a `destinationArn` names, and whether that service
//!   holds it ([`AccessLogDestination`], [`destination_is_absent`]). A subscription is an
//!   instruction to WRITE into that destination, so a destination the owning service does not
//!   hold is one no record would ever reach: it is refused with the modelled 404 rather than
//!   stored, exactly as a subscription of a resource this service does not hold is.

use std::collections::HashMap;
use std::time::SystemTime;

use super::{id_from_identifier, ComAmazonawsVpclatticeMercurycontrolplaneState, ResourceKind};

/// The ARN resource kind of an access log subscription:
/// `.../accesslogsubscription/als-…` (`AccessLogSubscriptionArn`'s modelled pattern).
pub const KIND_ACCESS_LOG_SUBSCRIPTION: &str = "accesslogsubscription";

/// The id prefix of an access log subscription (`^als-[0-9a-z]{17}$`).
pub const PREFIX_ACCESS_LOG_SUBSCRIPTION: &str = "als-";

/// How many access log subscriptions this emulator will hold for the account.
///
/// The real service caps subscriptions with a service quota, and
/// `CreateAccessLogSubscription` models no `ServiceQuotaExceededException` at all — its
/// modelled refusals are the conflict, the not-found and the validation error — so this is
/// NOT a fidelity claim about a published quota: it is the emulator's own ceiling, set
/// generously so no realistic workflow meets it, so that a caller looping on create cannot
/// grow this process's memory without bound. A create that would exceed it is refused with
/// the modelled `ConflictException`, the one modelled refusal of a create the service will
/// not carry out against state it already holds.
pub const MAX_ACCESS_LOG_SUBSCRIPTIONS: usize = 1_000;

/// One access log subscription of a service network, a service or a resource configuration.
///
/// Every member a read reports is stored: the identity this service minted, the resource the
/// subscription monitors (its id AND its ARN, both of which the responses model as required),
/// the destination records are delivered to, the log type the create asked for, and the two
/// instants of this service's clock. `AccessLogSubscriptionStatus` is not among them: the
/// model declares no status member on any of this resource's responses, so there is no
/// transient state for a clock to owe it.
#[derive(Debug, Clone)]
pub struct AccessLogSubscription {
    pub id: String,
    pub arn: String,
    /// The id of the service network, service or resource configuration being monitored,
    /// which `resourceId` reports.
    pub resource_id: String,
    /// That same resource's ARN, which `resourceArn` reports — built once with the
    /// subscription so every read reports the ARN the create resolved.
    pub resource_arn: String,
    /// The destination ARN exactly as the create (or the last update) stated it: the
    /// destination is another service's resource, so what this service reports is the handle
    /// the caller pointed it at.
    pub destination_arn: String,
    /// `serviceNetworkLogType` as the create set it, and absent when the create sent none:
    /// the member is optional on every response that models it and the model documents no
    /// default, so nothing is invented for it here.
    pub service_network_log_type: Option<String>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub last_updated_at: SystemTime,
    /// Creation order, so a listing pages deterministically. See [`super::encode_cursor`].
    pub sequence: u64,
}

/// The kinds a `resourceIdentifier` can name, as that member's own modelled pattern
/// enumerates them: a service network (`sn-`), a service (`svc-`) or a resource
/// configuration (`rcfg-`) — in the bare id or the ARN form
/// (`smithy_query(query="shape",shape="ResourceIdentifier")`).
pub const SUBSCRIBABLE_KINDS: [ResourceKind; 3] = [
    ResourceKind::ServiceNetwork,
    ResourceKind::Service,
    ResourceKind::ResourceConfiguration,
];

/// The kind an access-log refusal names when the submitted `resourceIdentifier` names no
/// kind a subscription can monitor at all: the first kind that member enumerates.
pub const UNRESOLVED_SUBSCRIBED_KIND: ResourceKind = SUBSCRIBABLE_KINDS[0];

/// The kind and id a `resourceIdentifier` names, or `None` when it names no kind a
/// subscription can monitor.
///
/// Both forms the member admits are read: a bare id, whose PREFIX says which kind it is, and
/// an ARN, whose kind path segment and id prefix have to agree. The id itself is read by the
/// shared reader ([`id_from_identifier`]), so an ARN of any partition, region or account
/// resolves to the resource it names.
pub fn subscribed_resource(identifier: &str) -> Option<(ResourceKind, String)> {
    match identifier.rsplit_once('/') {
        // The ARN form: the segment before the id is the kind, and it has to be the kind the
        // id's own prefix says it is.
        Some((head, tail)) => {
            let arn_kind = head.rsplit(['/', ':']).next()?;
            SUBSCRIBABLE_KINDS.into_iter().find_map(|kind| {
                (arn_kind == kind.arn_kind())
                    .then(|| id_from_identifier(tail, kind.id_prefix()))
                    .flatten()
                    .map(|id| (kind, id))
            })
        }
        // The bare-id form: the prefix is all there is, and it says which kind it is.
        None => SUBSCRIBABLE_KINDS
            .into_iter()
            .find_map(|kind| id_from_identifier(identifier, kind.id_prefix()).map(|id| (kind, id))),
    }
}

/// A destination an access log subscription delivers records to: one resource of one of the
/// three services `CreateAccessLogSubscription` documents as supported destinations
/// ("CloudWatch Log groups, Kinesis Data Firehose delivery streams, and Amazon S3 buckets"),
/// read off the ARN the caller pointed this service at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessLogDestination {
    /// A CloudWatch Logs log group, by the name its ARN carries. Both forms of that ARN name
    /// the same group, which is what the shared reading of one answers
    /// (`service_common::cloudwatch_logs::log_group_of_arn`).
    LogGroup(String),
    /// An Amazon Data Firehose delivery stream, by the name its ARN carries.
    DeliveryStream(String),
    /// An Amazon S3 bucket, by the name its ARN carries. A bucket ARN names the account's
    /// bucket and nothing else (`arn:<partition>:s3:::<bucket>`), so the resource field IS
    /// the name.
    Bucket(String),
}

/// The ARN namespace of each service that owns a supported destination, as that service's own
/// model states it (`awsService.arnNamespace`).
const LOGS_ARN_NAMESPACE: &str = "logs";
const FIREHOSE_ARN_NAMESPACE: &str = "firehose";
const S3_ARN_NAMESPACE: &str = "s3";

/// The resource type a Firehose delivery stream's ARN names
/// (`arn:<partition>:firehose:<region>:<account>:deliverystream/<name>`).
const DELIVERY_STREAM_RESOURCE_TYPE: &str = "deliverystream";

impl AccessLogDestination {
    /// The destination `arn` names, or `None` when it names no resource of a supported
    /// destination type — which the create and the update each answer with the modelled 404,
    /// since a destination nothing owns is one no record could ever reach.
    pub fn of_arn(arn: &str) -> Option<Self> {
        let parsed = ::service_common::arn::parse(arn)?;
        match parsed.vendor {
            LOGS_ARN_NAMESPACE => ::service_common::cloudwatch_logs::log_group_of_arn(arn)
                .map(|name| Self::LogGroup(name.to_string())),
            FIREHOSE_ARN_NAMESPACE => {
                ::service_common::arn::resource_id_of_type(arn, DELIVERY_STREAM_RESOURCE_TYPE)
                    .map(|name| Self::DeliveryStream(name.to_string()))
            }
            // A bucket ARN's resource field carries the bucket's name on its own; a key or an
            // access point under it is not a destination this service delivers to.
            S3_ARN_NAMESPACE => (!parsed.resource.contains('/')
                && !parsed.resource.contains(':'))
                .then(|| Self::Bucket(parsed.resource.to_string())),
            _ => None,
        }
    }
}

/// Whether the service that OWNS `destination` answered that it holds no such resource.
///
/// A subscription is an instruction to write into the destination, so the destination has to
/// resolve where its owning service holds it before the subscription is stored, or an
/// operator is left believing an audit trail exists where no record is ever written. Each
/// question is put through the shared reading of that service's own answer, so this service
/// and every other one that resolves such a destination agree about what "not there" means.
///
/// `false` — establish nothing — for every answer that is NOT the owning service saying the
/// resource is not there, which is what those shared readings already answer for an emulator
/// assembled without that service to ask.
///
/// The hop is the in-process callback into this same emulator, so nothing here opens a socket
/// or leaves the process. Call it with NO state guard held — it awaits.
pub async fn destination_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    destination: &AccessLogDestination,
) -> bool {
    match destination {
        AccessLogDestination::LogGroup(name) => {
            ::service_common::cloudwatch_logs::log_group_is_absent(ctx, region, name).await
        }
        AccessLogDestination::DeliveryStream(name) => {
            ::service_common::firehose_client::delivery_stream_is_absent(ctx, region, name).await
        }
        AccessLogDestination::Bucket(name) => {
            ::service_common::s3_client::bucket_is_absent(ctx, region, name).await
        }
    }
}

/// How the service refuses a `destinationArn` whose resource its owning service does not hold,
/// or that names no supported destination type at all:
/// `"AccessLogSubscription destination <destinationArn> cannot be found."`
///
/// The fixed text is `AccessLogSubscription destination ` … ` cannot be found.` and the
/// variable part is the destination ARN the REQUEST carried, so the message names back the
/// destination the caller pointed this service at. It is the same construction every
/// not-found message of this service follows ([`ResourceKind::not_found_message`]) over the
/// subscription's own resource name and the destination in place of an identifier of this
/// service's: the subscription could not be created because its DESTINATION is not there.
pub fn destination_not_found_message(destination_arn: &str) -> String {
    format!(
        "{} destination {destination_arn} cannot be found.",
        ResourceKind::AccessLogSubscription.message_name()
    )
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// The subscription `identifier` names, or `None` when this store holds none.
    ///
    /// Both forms the identifier member admits resolve through the shared reader, so a caller
    /// that holds the ARN and one that holds the bare id read the same record.
    pub fn access_log_subscription(&self, identifier: &str) -> Option<&AccessLogSubscription> {
        let id = id_from_identifier(identifier, PREFIX_ACCESS_LOG_SUBSCRIPTION)?;
        self.access_log_subscriptions.get(&id)
    }

    /// Every subscription monitoring the resource `resource_id`, in creation order — the one
    /// order a listing pages by.
    pub fn access_log_subscriptions_of(&self, resource_id: &str) -> Vec<&AccessLogSubscription> {
        let mut found: Vec<&AccessLogSubscription> = self
            .access_log_subscriptions
            .values()
            .filter(|subscription| subscription.resource_id == resource_id)
            .collect();
        found.sort_by_key(|subscription| subscription.sequence);
        found
    }

    /// Take away every subscription monitoring `resource_id`, answering how many went.
    ///
    /// "If you delete a service network, all resources related to the service network, such
    /// as the resource policy, auth policy, and access log subscriptions, are also deleted"
    /// (`DeleteServiceNetwork`'s own documentation), so a monitored resource's delete is what
    /// removes the subscriptions on it rather than leaving records nothing can resolve.
    pub fn remove_access_log_subscriptions_of(&mut self, resource_id: &str) -> usize {
        let doomed: Vec<String> = self
            .access_log_subscriptions
            .values()
            .filter(|subscription| subscription.resource_id == resource_id)
            .map(|subscription| subscription.id.clone())
            .collect();
        for id in &doomed {
            self.access_log_subscriptions.remove(id);
            // A clientToken record must not outlive the resource it created.
            self.idempotency_tokens.forget_resource(id);
        }
        doomed.len()
    }
}
