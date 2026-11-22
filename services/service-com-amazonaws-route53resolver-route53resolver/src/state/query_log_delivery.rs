// SPDX-License-Identifier: CC0-1.0

//! The RECORD of a query asked inside a VPC: what a Resolver query logging configuration
//! associated with that VPC delivers for the query, and where.
//!
//! "You can log the DNS queries that originate in your VPCs … create a query logging
//! configuration and associate it with one or more VPCs"
//! (`CreateResolverQueryLogConfig`/`AssociateResolverQueryLogConfig`). So a query this service's
//! data plane answered inside a VPC ([`super::vpc_dns`]) is recorded in every ACTIVE association
//! that VPC has, and the record goes to the configuration's own `DestinationArn`.
//!
//! # What a record carries
//!
//! The members the service publishes for a query log record, each read off the query that was
//! just answered or off the stores of the account:
//!
//! * `version`, the version of the record format ([`RECORD_VERSION`]);
//! * `account_id` and `region`, the account the configuration belongs to and the Region its
//!   destination is in;
//! * `vpc_id`, the VPC the query was asked in, and `srcaddr`, the private address it came from;
//! * `query_timestamp`, `query_name`, `query_type`, `query_class` and `rcode`, the question as
//!   the client asked it and the response code it was answered with;
//! * `answers`, one entry per record of the answer section, with its `Rdata`, `Type` and
//!   `Class`;
//! * `transport`, the protocol the query was carried over;
//! * `srcids`, the resources the query came from, for a carrier that is one of the account's;
//! * `firewall_rule_action`, `firewall_rule_group_id` and `firewall_domain_list_id` for a query a
//!   DNS Firewall rule MATCHED — this service's own rules, read through the same view a query is
//!   decided by ([`super::dns_firewall::FirewallView::matched`]), which is what makes an `ALERT`
//!   rule's match observable at all, since it changes no answer.
//!
//! Nothing here is synthesised: every member is a fact of the exchange the client just had, of
//! the configuration it is recorded in, or of the rule that decided it.
//!
//! # Where a record goes
//!
//! Into the CloudWatch Logs log group a `DestinationArn` of that service names — written into
//! CloudWatch Logs' OWN store in the SAME shared engine, through its own record types, so what
//! was delivered is exactly what `logs:FilterLogEvents`, `logs:GetLogEvents` and
//! `logs:DescribeLogStreams` report. The stream is named after the VPC the queries were asked
//! in, which is the resource the association is made for.
//!
//! A configuration delivering to one of the OTHER destinations the service takes (an Amazon S3
//! bucket, a Firehose delivery stream) is accepted by the control plane and records nothing
//! here: those two deliveries are writes to another service's data plane that this emulator does
//! not make yet, and reporting a delivery nothing performed would be worse than reporting none.
//!
//! Delivery is BEST EFFORT, as publishing to a customer-owned destination is: a log group
//! CloudWatch Logs does not hold receives nothing (nothing here creates one), and a group with no
//! room for another stream takes no further record. The query that produced the record is
//! answered either way — it is already answered when this is called — and the loss is traced.

use ::std::time::Instant;

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;
use ::service_common::vpc_private_dns::{InVpcQuerySource, LoggedQuery};

use super::dns_firewall::{qtype_number, MatchedFirewallRule};
use super::vpc_dns::StoredVpcDns;
use super::{
    now_rfc3339, QUERY_LOG_CONFIG_ASSOCIATION_STATUS_ACTIVE, QUERY_LOG_CONFIG_STATUS_CREATED,
};

/// The version of the query log record format this service writes, as the `version` member of a
/// record states it: the version AWS publishes for the Resolver query log format.
const RECORD_VERSION: &str = "1.100000";

/// The service whose ARNs name a CloudWatch Logs destination, and the resource type of a log
/// group inside one — the destination kind this module delivers into.
const LOGS_ARN_SERVICE: &str = "logs";
/// See [`LOGS_ARN_SERVICE`].
const LOG_GROUP_ARN_RESOURCE: &str = "log-group:";

impl StoredVpcDns {
    /// Record `query`, asked inside the VPC `source` names and already answered, in every query
    /// logging configuration the account has associated with that VPC.
    ///
    /// Call it with NO store guard held: it takes this service's store and releases it before
    /// the destination's own is taken.
    pub fn record_query(&self, source: &InVpcQuerySource<'_>, query: &LoggedQuery<'_>) {
        let destinations = self.logging_destinations(source.vpc_id);
        if destinations.is_empty() {
            return;
        }
        // The rule that decided a query for this name, as a record of it names one: this
        // service's own rules, read here at the instant the record is written. The decision the
        // client was answered with was read by the component that carried the query, so a rule
        // the account changes between the answer and the record is one the two readings can
        // differ on — the same window every other per-query reading of a live store is open to,
        // and no record is held back for it.
        let matched = self.firewall.as_ref().and_then(|firewall| {
            firewall
                .view_for_vpc(source.vpc_id)
                .matched(query.name, qtype_number(query.record_type))
        });
        for destination in destinations {
            let Some(log_group) = log_group_of(&destination) else {
                // A destination of another kind: see this module's docs.
                continue;
            };
            let region = arn_region(&destination).unwrap_or_default();
            let record = record_of(source, query, matched.as_ref(), &region);
            self.deliver_into_log_group(&region, log_group, source.vpc_id, &record);
        }
    }

    /// The `DestinationArn` of every configuration recording the queries of `vpc_id`: those
    /// whose association with the VPC is ACTIVE and whose configuration itself has been CREATED,
    /// because a configuration or an association still being provisioned is not logging yet.
    fn logging_destinations(&self, vpc_id: &str) -> Vec<String> {
        let now = Instant::now();
        let store = self.resolver.lock();
        store
            .resolver_query_log_configs
            .values()
            .filter(|config| config.status_at(now) == QUERY_LOG_CONFIG_STATUS_CREATED)
            .filter(|config| {
                config.association_for_vpc(vpc_id).is_some_and(|association| {
                    association.status_at(now) == QUERY_LOG_CONFIG_ASSOCIATION_STATUS_ACTIVE
                })
            })
            .map(|config| config.destination_arn.clone())
            .collect()
    }

    /// Write one record into the log group `log_group` of CloudWatch Logs' own store, in the
    /// stream named after the VPC the query was asked in.
    ///
    /// Best effort, for the reason this module's docs give.
    fn deliver_into_log_group(
        &self,
        region: &str,
        log_group: &str,
        vpc_id: &str,
        record: &str,
    ) {
        let Some(state) = self.logs.as_ref() else {
            // No CloudWatch Logs store on this engine: the destination cannot be reached, and
            // the query it was to record has already been answered.
            return;
        };
        let mut store = state.lock();
        let ingested_at = logs::now_ms();
        // The data identifiers, the key and the transformer in force for the destination group
        // are what that group does to everything it takes IN, by whichever path a record
        // arrives, so a delivered record is stored exactly as an ingested one is.
        let data_identifiers = store
            .log_groups
            .get(log_group)
            .map(|group| store.data_protection_identifiers_for(group))
            .unwrap_or_default();
        let ingest_kms_key_id = store
            .log_groups
            .get(log_group)
            .and_then(|group| group.kms_key_id.clone());
        let transformer = store
            .log_groups
            .get(log_group)
            .map(|group| logs::active_transformer::pipeline(&store, group))
            .unwrap_or_default();
        let stored = logs::StoredEvent {
            timestamp: ingested_at,
            masked_message: logs::data_protection::masked(record, &data_identifiers),
            transformed_message: transformer.transform(record),
            message: record.to_string(),
            ingestion_time: ingested_at,
            ingestion_seq: store.take_ingestion_seq(),
            ingest_kms_key_id,
        };
        let Some(group) = store.log_groups.get_mut(log_group) else {
            ::tracing::warn!(
                log_group = %log_group,
                "the log group a query logging configuration delivers into is not held by \
                 CloudWatch Logs, so a Resolver query log record was not delivered"
            );
            return;
        };
        let stream_name = vpc_id.to_string();
        // The stream is admitted against the ceiling the owning service's own `CreateLogStream`
        // admits one against, so no write path can take a group past it.
        if !group.streams.contains_key(&stream_name)
            && group.streams.len() >= logs::MAX_STREAMS_PER_GROUP
        {
            ::tracing::warn!(
                log_group = %log_group,
                log_stream = %stream_name,
                "the log group already holds the maximum number of streams, so a Resolver query \
                 log record was not delivered"
            );
            return;
        }
        let stream = group
            .streams
            .entry(stream_name.clone())
            .or_insert_with(|| {
                logs::LogStreamRecord::new(region, log_group, &stream_name, ingested_at)
            });
        stream.events.push(stored);
        // The stream is kept in the order its reader pages it in; records arrive in that order
        // already, so only one predating what is there moves.
        stream.events.sort_by_key(logs::StoredEvent::key);
        stream.enforce_storage_ceiling();
        stream.refresh_event_times();
        stream.last_ingestion_time = Some(ingested_at);
    }
}

/// The log group a `DestinationArn` names, and `None` for an ARN naming a destination of another
/// kind.
///
/// The stored form carries no `:*` stream wildcard ([`super::canonical_query_log_destination`]),
/// so what follows the resource type is the group's own name.
fn log_group_of(destination_arn: &str) -> Option<&str> {
    let fields: Vec<&str> = destination_arn.splitn(6, ':').collect();
    let [_arn, _partition, service, _region, _account, resource] = fields.as_slice() else {
        return None;
    };
    if *service != LOGS_ARN_SERVICE {
        return None;
    }
    let name = resource.strip_prefix(LOG_GROUP_ARN_RESOURCE)?;
    let name = name.strip_suffix(":*").unwrap_or(name);
    (!name.is_empty()).then_some(name)
}

/// The Region field of an ARN, and `None` for a value that is no ARN or names no Region.
fn arn_region(arn: &str) -> Option<String> {
    let region = arn.split(':').nth(3)?;
    (!region.is_empty()).then(|| region.to_string())
}

/// ONE query log record, as the destination holds it: the members this module's docs list, as a
/// JSON object.
fn record_of(
    source: &InVpcQuerySource<'_>,
    query: &LoggedQuery<'_>,
    matched: Option<&MatchedFirewallRule>,
    region: &str,
) -> String {
    let mut record = ::serde_json::Map::new();
    let mut put = |member: &str, value: ::serde_json::Value| {
        record.insert(member.to_string(), value);
    };
    put("version", RECORD_VERSION.into());
    put("account_id", ::engine::emulator_account_id().into());
    put("region", region.into());
    put("vpc_id", source.vpc_id.into());
    put("query_timestamp", query_timestamp().into());
    put("query_name", fully_qualified(query.name).into());
    put("query_type", query.record_type.into());
    put("query_class", query.class.into());
    put("rcode", query.response_code.into());
    put(
        "answers",
        query
            .answers
            .iter()
            .map(|answer| {
                ::serde_json::json!({
                    "Rdata": answer.rdata,
                    "Type": answer.record_type,
                    "Class": answer.class,
                })
            })
            .collect::<Vec<::serde_json::Value>>()
            .into(),
    );
    put("srcaddr", source.source_ip.into());
    put("transport", query.transport.to_ascii_uppercase().into());
    if !query.source_ids.is_empty() {
        let mut ids = ::serde_json::Map::new();
        for (kind, id) in query.source_ids {
            ids.insert((*kind).to_string(), (*id).into());
        }
        put("srcids", ids.into());
    }
    if let Some(matched) = matched {
        put("firewall_rule_action", matched.action.clone().into());
        put(
            "firewall_rule_group_id",
            matched.rule_group_id.clone().into(),
        );
        put(
            "firewall_domain_list_id",
            matched.domain_list_id.clone().into(),
        );
    }
    ::serde_json::Value::Object(record).to_string()
}

/// The instant the query was asked, as the `query_timestamp` member states one: the RFC 3339
/// UTC form every other instant this service reports is written in.
fn query_timestamp() -> String {
    now_rfc3339()
}

/// `name` as the `query_name` member states the name that was asked for: FULLY QUALIFIED, with
/// the dot that spells the root explicitly, which is the form a record of a query carries it in
/// whether or not the client's own question spelled it.
fn fully_qualified(name: &str) -> String {
    let named = name.trim();
    if named.is_empty() || named.ends_with('.') {
        return named.to_string();
    }
    format!("{named}.")
}
