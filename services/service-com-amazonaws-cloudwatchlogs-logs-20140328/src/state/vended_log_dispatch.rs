// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY LEG of a vended-log pipeline: which deliveries carry the logs one sending
//! resource produces, WHERE each of them writes them, and how each writes one record.
//!
//! The three control-plane calls a client makes — `PutDeliverySource` ("a logical object that
//! represents the resource that is actually sending the logs"), `PutDeliveryDestination` ("a
//! logical object that represents the actual delivery destination") and `CreateDelivery` ("a
//! delivery [...] pairing exactly one delivery source and one delivery destination") — wire up
//! a pipeline this service holds but another service FEEDS: the records travel from the
//! resource the source names. So the sending service needs one question answered, and this
//! module is where it is answered: for the resource it just produced a log record about, which
//! deliveries carry that record, where each writes it, and under which record layout.
//!
//! The answer is read from this service's OWN store — the delivery sources, destinations and
//! deliveries the three calls above wrote — so a client that built the pipeline through the
//! API gets records, and a client that built nothing gets none.
//!
//! # Which pipeline a resource's records travel
//!
//! A delivery source, a delivery destination and a delivery are REGIONAL resources
//! ([`super::RegionalKey`]), so the pipeline of one resource lives in one Region's endpoint:
//! the Region a source for it can be created in at all ([`super::delivery_source_required_region`]
//! pins it for a global sender), and otherwise the Region the resource's own ARN names. A
//! source matches when it REPRESENTS the resource — its `resourceArns` holds that ARN and its
//! `logType` is the type being sent — which is the same reading `PutDeliverySource` refuses a
//! duplicate by ([`super::ComAmazonawsCloudwatchlogsLogs20140328State::delivery_source_representing`]).
//! Each delivery naming such a source is one pipeline.
//!
//! # Where one delivery writes
//!
//! A destination is "an Amazon Web Services resource that represents an Amazon Web Services
//! service that logs can be sent to", and `PutDeliveryDestination` names the three kinds that
//! RECEIVE LOG RECORDS: "a log group in CloudWatch Logs, an Amazon S3 bucket, or a delivery
//! stream in Firehose". A delivery to any of those three is one THIS module resolves and
//! writes ([`VendedLogDestination`]) — a sender's records fan out to every destination its
//! deliveries name, whichever kinds those are, so a client that pointed one delivery at a
//! bucket and another at a delivery stream gets the same record in both. The X-Ray trace
//! destination ([`super::TRACE_DELIVERY_DESTINATION_TYPE`]) receives spans rather than log
//! records and carries no destination resource at all, so it is left to the service that owns
//! traces.
//!
//! # What one record looks like
//!
//! A delivery carries "the list of record fields to be delivered to the destination, in order"
//! and, for the text formats, "the field delimiter [...] used between record fields" — both
//! already resolved to what the delivery will deliver under, defaults included
//! ([`super::DeliveryRecord::resolve_configuration`]). So a record is the delivery's own field
//! list, in its own order, rendered in its destination's `outputFormat`: the structured format
//! ([`STRUCTURED_OUTPUT_FORMAT`]) as one JSON object of field name to value, and the text
//! formats ([`DELIMITED_OUTPUT_FORMATS`]) as the values joined by the delivery's delimiter.
//! The VALUES are the sending service's: this module asks it for one field at a time and
//! renders what it answers, so nothing about the request a record describes is invented here.
//!
//! # How the record gets there
//!
//! Each kind of destination is read by its consumer in its own way, so a record has to arrive
//! as that consumer reads it ([`deliver_record`]): a log group takes the record as one log
//! event through this service's own ingest path ([`::service_common::cloudwatch_logs::delivered`]),
//! a bucket takes it as an OBJECT a reader lists and fetches, and a delivery stream takes it as
//! one PUT RECORD, which that stream then delivers to its own destination under its own
//! buffering — the latter two through the one write this tree holds for each
//! ([`::service_common::vended_logs`]). What this module decides is WHICH destination and, for a
//! bucket, under which object key; the writes themselves are shared with every other sender.
//!
//! Delivery is BEST EFFORT, as it is for every vended-logs destination: the sender has already
//! answered whatever produced the record, so a destination the account has since deleted or a
//! write the owning service refuses loses the record and is reported in tracing rather than
//! failing anything.

use std::time::SystemTime;

use super::*;

/// The first segment of every delivered log-file key, which is the prefix an audit pipeline
/// reads a bucket of vended logs under — the root `destinationResourceArn`'s own
/// documentation points at ("how the destination prefix, suffix path, and Hive-compatible
/// setting determine the Amazon S3 object key").
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";

/// The partition key the Hive-compatible layout names the ACCOUNT by:
/// `enableHiveCompatiblePath` "causes the S3 objects that contain delivered logs to use a
/// prefix structure that allows for integration with Apache Hive", which is the `name=value`
/// form of each fixed segment of the layout.
const HIVE_ACCOUNT_PARTITION: &str = "aws-account-id";

/// The partition key the Hive-compatible layout names the SENDING SERVICE by; see
/// [`HIVE_ACCOUNT_PARTITION`].
const HIVE_SERVICE_PARTITION: &str = "aws-service";

/// What a delivered log file's name ends with: the delivery's records, gzipped — which is what
/// the write that frames them produces ([`::service_common::vended_logs::delivered_object`]).
const LOG_FILE_SUFFIX: &str = ".log.gz";

/// What identifies the SENDER whose records one delivery carries: the service sending them,
/// the kind of resource they come from, that resource's own id, and the Region the pipeline
/// lives in.
///
/// Read off the delivery source's `resourceArns` — "the ARN of the Amazon Web Services
/// resource that sends logs and is represented by this delivery source" — so it describes the
/// resource the account named rather than anything the sender restates. It is what a
/// destination needs in order to keep one sender's records apart from another's: the stream a
/// log group's records are written into, and the object layout a bucket's are written under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendedLogSender {
    /// `DeliverySource$service` — "The Amazon Web Services service that is sending logs", as
    /// [`super::delivery_source_service`] reads it off the resource ARN.
    pub service: String,
    /// The kind of resource within that service (`distribution`, `event-bus`, …).
    pub resource_type: String,
    /// The sending resource's own id, as its ARN names it.
    pub resource_id: String,
    /// The Region the delivery pipeline of that resource lives in
    /// ([`delivery_pipeline_region`]).
    pub region: String,
}

/// Where one delivery writes the records it carries — one variant per destination kind that
/// receives log records ("a log group in CloudWatch Logs, an Amazon S3 bucket, or a delivery
/// stream in Firehose").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VendedLogDestination {
    /// `CWL`: the log group the destination's `destinationResourceArn` names.
    LogGroup {
        /// The group's name, as [`::service_common::cloudwatch_logs::log_group_of_arn`] reads
        /// it off that ARN.
        log_group: String,
    },
    /// `S3`: the bucket the destination names, and the object layout the delivery writes
    /// under.
    Bucket {
        /// The bucket's name, which is the first segment of the ARN's resource part.
        bucket: String,
        /// The DESTINATION PREFIX: anything the destination's ARN names beneath the bucket,
        /// which the whole object layout is written under. Empty where the ARN names the
        /// bucket alone.
        prefix: String,
        /// `S3DeliveryConfiguration$suffixPath` — "re-configuring the S3 object prefix to
        /// contain either static or variable sections", as the delivery will deliver under it.
        suffix_path: String,
        /// `S3DeliveryConfiguration$enableHiveCompatiblePath`, as the delivery will deliver
        /// under it.
        hive_compatible_path: bool,
    },
    /// `FH`: the Firehose delivery stream the destination names.
    DeliveryStream {
        /// The stream's name, as the ARN's `deliverystream` resource names it.
        delivery_stream: String,
    },
}

/// One delivery that carries a sending resource's vended logs: where its records go, and the
/// configuration they are written under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendedLogDelivery {
    /// `Delivery$id` — which delivery wrote the record, for the sender's own tracing.
    pub delivery_id: String,
    /// `DeliverySource$name` — the source this delivery is paired with.
    pub delivery_source_name: String,
    /// Where this delivery writes.
    pub destination: VendedLogDestination,
    /// The Region the destination resource is addressed in, which is the endpoint the write is
    /// made on.
    pub region: String,
    /// `DeliveryDestination$outputFormat` — "the format of the logs that are sent to this
    /// delivery destination", defaulted to the structured format for a destination that
    /// states none.
    pub output_format: String,
    /// `Delivery$recordFields`, as the delivery will deliver them: in order, defaults
    /// included.
    pub record_fields: Vec<String>,
    /// `Delivery$fieldDelimiter`, as the delivery will deliver under it.
    pub field_delimiter: String,
    /// The resource whose records this delivery carries.
    pub sender: VendedLogSender,
}

impl VendedLogDelivery {
    /// The record this delivery writes for one thing the sender logged, with `value` asked for
    /// each of its record fields in turn.
    ///
    /// The field list and its order are the delivery's ([`Self::record_fields`]); the
    /// encoding is its destination's `outputFormat`. A field the sender has no value for is
    /// whatever the sender answers for it — the placeholder its own records carry — never a
    /// value composed here.
    #[must_use]
    pub fn record(&self, value: impl Fn(&str) -> String) -> String {
        rendered_record(
            &self.output_format,
            &self.record_fields,
            &self.field_delimiter,
            value,
        )
    }

    /// This delivery as a LOG GROUP delivery, or `None` when it writes somewhere else.
    ///
    /// The view a sender that delivers into log groups alone reads its pipeline through
    /// ([`log_group_deliveries`]).
    #[must_use]
    pub fn into_log_group_delivery(self) -> Option<LogGroupDelivery> {
        let VendedLogDestination::LogGroup { log_group } = &self.destination else {
            return None;
        };
        Some(LogGroupDelivery {
            log_group: log_group.clone(),
            region: self.region.clone(),
            delivery_id: self.delivery_id.clone(),
            delivery_source_name: self.delivery_source_name.clone(),
            output_format: self.output_format.clone(),
            record_fields: self.record_fields.clone(),
            field_delimiter: self.field_delimiter.clone(),
        })
    }
}

/// One delivery that carries a sending resource's vended logs into a CloudWatch Logs log
/// group: the [`VendedLogDelivery`] view a sender that delivers into log groups alone reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogGroupDelivery {
    /// `Delivery$id` — which delivery wrote the record, for the sender's own tracing.
    pub delivery_id: String,
    /// `DeliverySource$name` — the source this delivery is paired with.
    pub delivery_source_name: String,
    /// The name of the log group the destination's `destinationResourceArn` names.
    pub log_group: String,
    /// The Region that log group's ARN names, which is the endpoint the write is made on.
    pub region: String,
    /// `DeliveryDestination$outputFormat` — "the format of the logs that are sent to this
    /// delivery destination", defaulted to the structured format for a destination that
    /// states none, which is the one format a log-group destination receives.
    pub output_format: String,
    /// `Delivery$recordFields`, as the delivery will deliver them: in order, defaults
    /// included.
    pub record_fields: Vec<String>,
    /// `Delivery$fieldDelimiter`, as the delivery will deliver under it.
    pub field_delimiter: String,
}

impl LogGroupDelivery {
    /// The record this delivery writes for one thing the sender logged; see
    /// [`VendedLogDelivery::record`], whose rendering this is.
    #[must_use]
    pub fn record(&self, value: impl Fn(&str) -> String) -> String {
        rendered_record(
            &self.output_format,
            &self.record_fields,
            &self.field_delimiter,
            value,
        )
    }
}

/// ONE record of `record_fields`, in that order, encoded as `output_format` writes one: the
/// structured format as a JSON object of field name to value, and a text format as the values
/// joined by `field_delimiter`.
///
/// The one rendering both views of a delivery report through, so a record cannot be written two
/// ways.
fn rendered_record(
    output_format: &str,
    record_fields: &[String],
    field_delimiter: &str,
    value: impl Fn(&str) -> String,
) -> String {
    if output_format == STRUCTURED_OUTPUT_FORMAT {
        let mut record = String::from("{");
        for (position, field) in record_fields.iter().enumerate() {
            if position > 0 {
                record.push(',');
            }
            record.push_str(&json_string(field));
            record.push(':');
            record.push_str(&json_string(&value(field)));
        }
        record.push('}');
        return record;
    }
    record_fields
        .iter()
        .map(|field| value(field))
        .collect::<Vec<String>>()
        .join(field_delimiter)
}

/// One JSON string literal, escaped as JSON escapes one, so a field name or a value carrying a
/// quote, a backslash or a control character cannot break the record that holds it.
fn json_string(value: &str) -> String {
    ::serde_json::Value::String(value.to_string()).to_string()
}

/// Every delivery that carries `log_type` logs of the resource `resource_arn` names, whichever
/// kind of destination each writes to.
///
/// The pipeline is read from this service's own store in the shared engine, so what a sender
/// gets back is what the account really created through `PutDeliverySource`,
/// `PutDeliveryDestination` and `CreateDelivery`. The answer is EMPTY — nothing is delivered —
/// for each of: an emulator with no CloudWatch Logs mounted, an account that wired no source
/// for this resource and log type, a source no delivery pairs, a destination whose
/// `destinationResourceArn` names no resource of a kind that receives log records, and a
/// delivery that carries no record field (there being no record to write for it).
///
/// Takes no lock of the CALLER's service: it locks this service's store for the read and
/// releases it before answering, so a sender resolves its pipeline with its own store free and
/// then makes the writes.
#[must_use]
pub fn vended_log_deliveries<Eng: Send + Sync + 'static>(
    engine: &Eng,
    resource_arn: &str,
    log_type: &str,
) -> Vec<VendedLogDelivery> {
    let Some(region) = delivery_pipeline_region(resource_arn) else {
        return Vec::new();
    };
    let Some(sender) = sender_of(resource_arn, &region) else {
        return Vec::new();
    };
    let Some(state) = super::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    // Which SOURCES represent this resource for this log type: a source's `resourceArns`
    // "contains the ARN of the Amazon Web Services resource that sends logs and is represented
    // by this delivery source", and its `logType` is "the type of log that the source is
    // sending".
    let sources: Vec<&str> = store
        .delivery_sources_in(&region)
        .filter(|source| {
            source.log_type == log_type
                && source
                    .resource_arns
                    .iter()
                    .any(|named| named == resource_arn)
        })
        .map(|source| source.name.as_str())
        .collect();
    if sources.is_empty() {
        return Vec::new();
    }
    store
        .deliveries_in(&region)
        .filter(|delivery| {
            sources
                .iter()
                .any(|name| *name == delivery.delivery_source_name)
        })
        .filter_map(|delivery| {
            let destination =
                store.delivery_destination_by_arn(&region, &delivery.delivery_destination_arn)?;
            let (written_to, destination_region) =
                destination_of(destination, delivery, &region)?;
            let record_fields = delivery.record_fields.clone().unwrap_or_default();
            if record_fields.is_empty() {
                return None;
            }
            Some(VendedLogDelivery {
                delivery_id: delivery.id.clone(),
                delivery_source_name: delivery.delivery_source_name.clone(),
                destination: written_to,
                region: destination_region,
                output_format: destination
                    .output_format
                    .clone()
                    .unwrap_or_else(|| STRUCTURED_OUTPUT_FORMAT.to_string()),
                record_fields,
                field_delimiter: delivery.field_delimiter.clone().unwrap_or_default(),
                sender: sender.clone(),
            })
        })
        .collect()
}

/// Every delivery of [`vended_log_deliveries`] that writes into a log group, in the view a
/// sender delivering into log groups alone reads its pipeline through.
#[must_use]
pub fn log_group_deliveries<Eng: Send + Sync + 'static>(
    engine: &Eng,
    resource_arn: &str,
    log_type: &str,
) -> Vec<LogGroupDelivery> {
    vended_log_deliveries(engine, resource_arn, log_type)
        .into_iter()
        .filter_map(VendedLogDelivery::into_log_group_delivery)
        .collect()
}

/// Where the delivery `delivery` writes, and the Region that destination is addressed in, or
/// `None` for a destination that receives no log record.
///
/// The three kinds that do are the ones `PutDeliveryDestination` names ("a log group in
/// CloudWatch Logs, an Amazon S3 bucket, or a delivery stream in Firehose"). Each is addressed
/// where its OWN ARN says: the group and the stream carry a Region, held to the grammar a
/// Region is written in before it addresses an endpoint ([`::service_common::arn::region_of`]),
/// so an ARN naming no resource of a Region addresses no endpoint and nothing is written for
/// it. A BUCKET ARN carries neither account nor Region, so the object is written on the
/// endpoint of the Region the pipeline itself lives in.
fn destination_of(
    destination: &DeliveryDestinationRecord,
    delivery: &DeliveryRecord,
    pipeline_region: &str,
) -> Option<(VendedLogDestination, String)> {
    let destination_arn = destination.destination_resource_arn.as_deref()?;
    match destination.delivery_destination_type.as_deref()? {
        CWL_DELIVERY_DESTINATION_TYPE => {
            let log_group = ::service_common::cloudwatch_logs::log_group_of_arn(destination_arn)?;
            let region =
                ::service_common::arn::region_of(destination_arn, CLOUDWATCH_LOGS_VENDOR)?;
            Some((
                VendedLogDestination::LogGroup {
                    log_group: log_group.to_string(),
                },
                region.to_string(),
            ))
        }
        S3_DELIVERY_DESTINATION_TYPE => {
            let resource = ::service_common::arn::parse(destination_arn)?.resource;
            let (bucket, prefix) = resource
                .split_once('/')
                .unwrap_or((resource, ""));
            if bucket.is_empty() {
                return None;
            }
            let configured = delivery.s3_delivery_configuration.as_ref();
            Some((
                VendedLogDestination::Bucket {
                    bucket: bucket.to_string(),
                    prefix: prefix.trim_matches('/').to_string(),
                    suffix_path: configured
                        .and_then(|configured| configured.suffix_path.clone())
                        .unwrap_or_default(),
                    hive_compatible_path: configured
                        .and_then(|configured| configured.enable_hive_compatible_path)
                        .unwrap_or(CONFIGURATION_TEMPLATE_DEFAULT_HIVE_COMPATIBLE_PATH),
                },
                pipeline_region.to_string(),
            ))
        }
        STREAM_DELIVERY_DESTINATION_TYPE => {
            let delivery_stream = ::service_common::arn::resource_id_of_type(
                destination_arn,
                DELIVERY_STREAM_RESOURCE_TYPE,
            )?;
            let region = ::service_common::arn::region_of(destination_arn, STREAM_VENDOR)?;
            Some((
                VendedLogDestination::DeliveryStream {
                    delivery_stream: delivery_stream.to_string(),
                },
                region.to_string(),
            ))
        }
        // The X-Ray trace destination receives spans rather than log records, and every other
        // value is a destination this service does not resolve.
        _ => None,
    }
}

/// The ARN service namespace of Amazon Data Firehose, the vendor whose
/// `destinationResourceArn` must name a delivery stream (the `firehose` vendor of
/// [`super::DELIVERY_DESTINATION_VENDOR_TYPES`]).
const STREAM_VENDOR: &str = "firehose";

/// The resource type a Firehose delivery stream's ARN carries.
const DELIVERY_STREAM_RESOURCE_TYPE: &str = "deliverystream";

/// What the resource ARN `resource_arn` says about the SENDER of the records, for a pipeline
/// held in `region`.
///
/// `None` for a value that is no ARN: nothing then names a sending resource.
fn sender_of(resource_arn: &str, region: &str) -> Option<VendedLogSender> {
    let arn = ::service_common::arn::parse(resource_arn)?;
    let resource_type = arn.resource_type();
    // The id is what the resource part names BENEATH its type, whichever separator the ARN
    // writes (`distribution/E1EXAMPLE`, `event-bus/default`), and ONLY that one separator — a
    // log group's name opens with one of its own (`log-group:/vended/one`). A resource part that
    // is the bare name of the resource (an Amazon S3 bucket) is itself the id.
    let resource_id = arn
        .resource
        .strip_prefix(resource_type)
        .map(|rest| {
            rest.strip_prefix(':')
                .or_else(|| rest.strip_prefix('/'))
                .unwrap_or(rest)
        })
        .filter(|id| !id.is_empty())
        .unwrap_or(arn.resource);
    Some(VendedLogSender {
        service: arn.vendor.to_string(),
        resource_type: resource_type.to_string(),
        resource_id: resource_id.to_string(),
        region: region.to_string(),
    })
}

/// Deliver ONE record of the sender's to the destination `delivery` names, as the consumer of
/// that kind of destination reads it.
///
/// `at` is the instant the record describes, which is what the object layout of a bucket
/// destination partitions by. `true` once the record is in the destination, `false` when it
/// could not be written — which is never a failure of whatever the sender was answering: see
/// the module docs.
///
/// Call it with NO state guard held, of this service or of the sender's: it awaits the write
/// into the service that owns the destination.
pub async fn deliver_record(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    delivery: &VendedLogDelivery,
    at: SystemTime,
    record: &str,
) -> bool {
    // The record travels as the one-element batch each write below takes, so one record and a
    // batch of them go in by the same path.
    let records = [record.to_string()];
    match &delivery.destination {
        VendedLogDestination::LogGroup { log_group } => {
            ::service_common::cloudwatch_logs::delivered(
                ctx,
                &delivery.region,
                log_group,
                &log_stream_name(&delivery.sender),
                &records,
            )
            .await
        }
        VendedLogDestination::Bucket {
            bucket,
            prefix,
            suffix_path,
            hive_compatible_path,
        } => {
            let key = delivered_object_key(
                prefix,
                *hive_compatible_path,
                suffix_path,
                delivery,
                at,
                record,
            );
            ::service_common::vended_logs::delivered_object(
                ctx,
                &delivery.region,
                bucket,
                &key,
                &records,
            )
            .await
        }
        VendedLogDestination::DeliveryStream { delivery_stream } => {
            ::service_common::vended_logs::delivered_to_stream(
                ctx,
                &delivery.region,
                delivery_stream,
                &records,
            )
            .await
        }
    }
}

/// The stream a sender's records are written into, inside a destination log group.
///
/// AWS publishes no name for the stream a vended-log delivery writes into, so the records of
/// one SENDER are kept in one stream of their own, named by the resource that sent them: a
/// group several resources deliver into keeps each one's records apart, and a client reading
/// the group — `FilterLogEvents` reads across every stream of it — sees all of them.
fn log_stream_name(sender: &VendedLogSender) -> String {
    sender.resource_id.clone()
}

/// The key one delivered object takes, which is what the member's own documentation describes
/// as decided by "the destination prefix, suffix path, and Hive-compatible setting":
/// `[<destination prefix>/]AWSLogs/<account-id>/<service>/<suffix path>/<file name>`, and
/// `aws-account-id=<account-id>/aws-service=<service>` in place of those two segments where
/// the delivery writes Hive-compatible prefixes.
///
/// The suffix path is the delivery's own, with its variable sections resolved
/// ([`resolved_suffix_path`]). The FILE NAME is composed from the fields that identify the
/// file — the account, the sending service, the Region, the sending resource, the minute the
/// records belong to, and a field that tells two files of one minute apart — as the sibling
/// vended-log layouts in this tree compose theirs (`service_common::trail_log_files`,
/// `service-com-amazonaws-wafv2-awswaf-20190729`'s delivered objects): AWS publishes no file
/// name for a delivery of this kind.
fn delivered_object_key(
    prefix: &str,
    hive_compatible_path: bool,
    suffix_path: &str,
    delivery: &VendedLogDelivery,
    at: SystemTime,
    record: &str,
) -> String {
    let account = ::engine::emulator_account_id();
    let sender = &delivery.sender;
    let under = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let root = if hive_compatible_path {
        format!(
            "{LOG_FILE_ROOT_SEGMENT}/{HIVE_ACCOUNT_PARTITION}={account}/\
             {HIVE_SERVICE_PARTITION}={}/",
            sender.service
        )
    } else {
        format!("{LOG_FILE_ROOT_SEGMENT}/{account}/{}/", sender.service)
    };
    let partition = resolved_suffix_path(suffix_path, delivery, at);
    let minute = ::chrono::DateTime::<::chrono::Utc>::from(at).format("%Y%m%dT%H%MZ");
    let unique = unique_field(delivery, at, record);
    format!(
        "{under}{root}{partition}{account}_{}_{}_{}_{minute}_{unique}{LOG_FILE_SUFFIX}",
        sender.service, sender.region, sender.resource_id
    )
}

/// `suffix_path` with its variable sections resolved, and a trailing separator where it names
/// a partition at all.
///
/// "This string allows re-configuring the S3 object prefix to contain either static or
/// variable sections. The valid variables to use in the suffix path vary by log type" — and
/// each sender publishes its own list ([`super::vended_log_templates`]). The variables resolved
/// here are the ones every list carries: the account the delivery is made in, the Region the
/// pipeline lives in, the four calendar fields of the instant the records belong to, and the
/// identifier of the sending resource — written either generically (`resource_id`) or as the
/// resource kind's own variable (`distribution_id` for a `distribution`). A section naming
/// anything else is left exactly as the delivery states it: the suffix path is the caller's own
/// text, and nothing here invents a value for a name no published list carries.
fn resolved_suffix_path(suffix_path: &str, delivery: &VendedLogDelivery, at: SystemTime) -> String {
    let sender = &delivery.sender;
    let at = ::chrono::DateTime::<::chrono::Utc>::from(at);
    let resolved = [
        (
            "accountid".to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
        ("region".to_string(), sender.region.clone()),
        ("yyyy".to_string(), at.format("%Y").to_string()),
        ("MM".to_string(), at.format("%m").to_string()),
        ("dd".to_string(), at.format("%d").to_string()),
        ("HH".to_string(), at.format("%H").to_string()),
        ("resource_id".to_string(), sender.resource_id.clone()),
        (
            format!("{}_id", sender.resource_type),
            sender.resource_id.clone(),
        ),
    ];
    let mut path = suffix_path.to_string();
    for (variable, value) in resolved {
        path = path.replace(&format!("{{{variable}}}"), &value);
    }
    let path = path.trim_matches('/');
    if path.is_empty() {
        return String::new();
    }
    format!("{path}/")
}

/// The field one delivered object's name ends with, which tells two objects of the same minute
/// apart: a stable mixing of the delivery that wrote it, the instant the record describes and
/// the record itself ([`::service_common::stable_hex_id`]).
///
/// DERIVED from the delivery and the record rather than drawn at random, so two records
/// delivered in the same minute are two objects and neither overwrites the other, two
/// deliveries that carry one record into the same bucket write two objects, and a delivery that
/// could be made twice names one object rather than two copies.
fn unique_field(delivery: &VendedLogDelivery, at: SystemTime, record: &str) -> String {
    let nanos = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    ::service_common::stable_hex_id(&[&delivery.delivery_id, &nanos.to_string(), record])
}

/// The Region whose endpoint holds the delivery pipeline of `resource_arn`: the one Region a
/// delivery source for it can be created in at all where the resource pins one
/// ([`super::delivery_source_required_region`]), and the Region the resource's own ARN names
/// otherwise.
///
/// `None` for a value that is no ARN, and for a regional resource whose ARN names no Region:
/// neither addresses an endpoint a pipeline could have been created through.
fn delivery_pipeline_region(resource_arn: &str) -> Option<String> {
    if let Some(pinned) = delivery_source_required_region(resource_arn) {
        return Some(pinned.to_string());
    }
    ::service_common::arn::parse(resource_arn)
        .map(|arn| arn.region)
        .filter(|region| !region.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A delivery whose records are JSON objects of the fields it carries, in its own order.
    fn structured(fields: &[&str]) -> LogGroupDelivery {
        LogGroupDelivery {
            delivery_id: "0000000000000001".to_string(),
            delivery_source_name: "sender".to_string(),
            log_group: "/vended/one".to_string(),
            region: "us-east-1".to_string(),
            output_format: STRUCTURED_OUTPUT_FORMAT.to_string(),
            record_fields: fields.iter().map(|field| (*field).to_string()).collect(),
            field_delimiter: String::new(),
        }
    }

    /// The same delivery, carrying its records to `destination` instead.
    fn carried_to(destination: VendedLogDestination) -> VendedLogDelivery {
        VendedLogDelivery {
            delivery_id: "0000000000000001".to_string(),
            delivery_source_name: "sender".to_string(),
            destination,
            region: "us-east-1".to_string(),
            output_format: STRUCTURED_OUTPUT_FORMAT.to_string(),
            record_fields: vec!["time".to_string()],
            field_delimiter: String::new(),
            sender: VendedLogSender {
                service: "cloudfront".to_string(),
                resource_type: "distribution".to_string(),
                resource_id: "E1EXAMPLE".to_string(),
                region: "us-east-1".to_string(),
            },
        }
    }

    /// An instant with a known UTC calendar rendering: 2026-06-26T02:22:39.000000075Z.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_782_440_559) + Duration::from_nanos(75)
    }

    #[test]
    fn a_structured_record_is_one_json_object_of_the_fields_in_order() {
        let delivery = structured(&["time", "status", "path"]);
        let record = delivery.record(|field| match field {
            "time" => "01:42:39".to_string(),
            "status" => "200".to_string(),
            _ => "/probe.txt".to_string(),
        });
        let expected = r#"{"time":"01:42:39","status":"200","path":"/probe.txt"}"#;
        assert_eq!(
            record, expected,
            "case the three fields in order: expected {expected}, got {record}"
        );
        let parsed: ::serde_json::Value = ::serde_json::from_str(&record)
            .unwrap_or_else(|error| panic!("the record is JSON ({error}): {record}"));
        assert_eq!(
            parsed["status"], "200",
            "case reading the record back: expected the status field, got {parsed}"
        );
    }

    #[test]
    fn a_value_carrying_the_framing_of_the_format_is_escaped_rather_than_breaking_the_record() {
        let delivery = structured(&["agent"]);
        let record = delivery.record(|_field| "a \"quoted\"\nagent".to_string());
        let parsed: ::serde_json::Value = ::serde_json::from_str(&record)
            .unwrap_or_else(|error| panic!("the record is JSON ({error}): {record}"));
        assert_eq!(
            parsed["agent"], "a \"quoted\"\nagent",
            "case a value carrying a quote and a line break: expected it read back whole, got \
             {parsed}"
        );
    }

    #[test]
    fn a_text_record_joins_the_values_with_the_deliverys_own_delimiter() {
        for (case, format, delimiter, expected) in [
            ("plain, tab delimited", "plain", "\t", "01:42:39\t200"),
            ("w3c, space delimited", "w3c", " ", "01:42:39 200"),
            ("raw, comma delimited", "raw", ",", "01:42:39,200"),
            ("no delimiter at all", "plain", "", "01:42:39200"),
        ] {
            let delivery = LogGroupDelivery {
                output_format: format.to_string(),
                field_delimiter: delimiter.to_string(),
                ..structured(&["time", "status"])
            };
            let record = delivery.record(|field| match field {
                "time" => "01:42:39".to_string(),
                _ => "200".to_string(),
            });
            assert_eq!(
                record, expected,
                "case {case}: expected {expected:?}, got {record:?}"
            );
        }
    }

    #[test]
    fn a_pipeline_is_looked_for_where_the_sending_resource_pins_it() {
        for (case, resource_arn, expected) in [
            (
                "a global sender, pinned to the partition's one endpoint",
                "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE",
                Some("us-east-1"),
            ),
            (
                "a regional sender, in its own Region",
                "arn:aws:elasticloadbalancing:eu-west-2:000000000000:loadbalancer/app/one/abc",
                Some("eu-west-2"),
            ),
            (
                "a resource whose ARN names no Region and whose service pins none",
                "arn:aws:s3:::vended-logs",
                None,
            ),
            ("not an ARN at all", "E1EXAMPLE", None),
        ] {
            let got = delivery_pipeline_region(resource_arn);
            assert_eq!(
                got.as_deref(),
                expected,
                "case {case}: expected {expected:?} for {resource_arn:?}, got {got:?}"
            );
        }
    }

    /// RULE: the sender is read off the resource ARN the delivery source represents —
    /// whichever separator that ARN writes its resource with.
    #[test]
    fn the_sender_is_read_off_the_resource_arn() {
        for (case, resource_arn, expected) in [
            (
                "a resource written with a slash",
                "arn:aws:cloudfront::000000000000:distribution/E1EXAMPLE",
                Some(("cloudfront", "distribution", "E1EXAMPLE")),
            ),
            (
                "a resource written with a colon",
                "arn:aws:logs:us-east-1:000000000000:log-group:/vended/one",
                Some(("logs", "log-group", "/vended/one")),
            ),
            (
                "a nested resource id",
                "arn:aws:elasticloadbalancing:eu-west-2:000000000000:loadbalancer/app/one/abc",
                Some(("elasticloadbalancing", "loadbalancer", "app/one/abc")),
            ),
            (
                "a resource part that is the bare name of the resource",
                "arn:aws:s3:::vended-logs",
                Some(("s3", "vended-logs", "vended-logs")),
            ),
            ("not an ARN at all", "E1EXAMPLE", None),
        ] {
            let got = sender_of(resource_arn, "us-east-1");
            let read = got.as_ref().map(|sender| {
                (
                    sender.service.as_str(),
                    sender.resource_type.as_str(),
                    sender.resource_id.as_str(),
                )
            });
            assert_eq!(
                read, expected,
                "case {case}: expected {expected:?} for {resource_arn:?}, got {read:?}"
            );
        }
    }

    /// RULE: a suffix path's variable sections are resolved to the delivery's own facts, and a
    /// section naming anything no published list carries is left as the delivery states it.
    #[test]
    fn a_suffix_path_resolves_the_published_variables() {
        let delivery = carried_to(VendedLogDestination::LogGroup {
            log_group: "/vended/one".to_string(),
        });
        for (case, template, expected) in [
            (
                "the layout a CloudFront delivery publishes",
                "{region}/{distribution_id}/{yyyy}/{MM}/{dd}/{HH}/",
                "us-east-1/E1EXAMPLE/2026/06/26/02/",
            ),
            (
                "the generic resource variable",
                "{region}/{resource_id}/",
                "us-east-1/E1EXAMPLE/",
            ),
            (
                "the account and the day alone",
                "{accountid}/{yyyy}/{MM}/{dd}",
                "000000000000/2026/06/26/",
            ),
            ("a static section", "edge/logs", "edge/logs/"),
            ("no suffix path at all", "", ""),
            (
                "a variable no published list carries",
                "{region}/{unpublished}/",
                "us-east-1/{unpublished}/",
            ),
        ] {
            let got = resolved_suffix_path(template, &delivery, at());
            assert_eq!(
                got, expected,
                "case {case}: expected {expected:?} for {template:?}, got {got:?}"
            );
        }
    }

    /// RULE: the key is the destination prefix, the delivered-logs root naming the account and
    /// the sending service, the resolved suffix path, and the file's own name — with the
    /// Hive-compatible setting deciding how the two fixed segments are written.
    #[test]
    fn a_delivered_object_key_carries_the_destination_prefix_root_and_suffix_path() {
        let delivery = carried_to(VendedLogDestination::Bucket {
            bucket: "vended-logs".to_string(),
            prefix: String::new(),
            suffix_path: String::new(),
            hive_compatible_path: false,
        });
        let record = r#"{"x-edge-request-id":"P6SGg5LnS"}"#;
        let name = format!(
            "000000000000_cloudfront_us-east-1_E1EXAMPLE_20260626T0222Z_{}.log.gz",
            unique_field(&delivery, at(), record)
        );
        for (case, prefix, hive, suffix_path, expected) in [
            (
                "no destination prefix, no suffix path",
                "",
                false,
                "",
                format!("AWSLogs/000000000000/cloudfront/{name}"),
            ),
            (
                "the suffix path a CloudFront delivery publishes",
                "",
                false,
                "{region}/{distribution_id}/{yyyy}/{MM}/{dd}/{HH}/",
                format!(
                    "AWSLogs/000000000000/cloudfront/us-east-1/E1EXAMPLE/2026/06/26/02/{name}"
                ),
            ),
            (
                "a destination prefix ahead of the whole layout",
                "edge",
                false,
                "{region}/",
                format!("edge/AWSLogs/000000000000/cloudfront/us-east-1/{name}"),
            ),
            (
                "Hive-compatible prefixes",
                "",
                true,
                "{region}/",
                format!(
                    "AWSLogs/aws-account-id=000000000000/aws-service=cloudfront/us-east-1/{name}"
                ),
            ),
        ] {
            let got = delivered_object_key(prefix, hive, suffix_path, &delivery, at(), record);
            assert_eq!(
                got, expected,
                "case {case}: expected {expected:?}, got {got:?}"
            );
        }
    }

    /// RULE: the field that tells two delivered objects apart is derived from the delivery, the
    /// instant and the record — so two records never name one object, and the same record named
    /// twice names the same object.
    #[test]
    fn the_unique_field_tells_two_delivered_records_apart() {
        let delivery = carried_to(VendedLogDestination::Bucket {
            bucket: "vended-logs".to_string(),
            prefix: String::new(),
            suffix_path: String::new(),
            hive_compatible_path: false,
        });
        let sibling = VendedLogDelivery {
            delivery_id: "0000000000000002".to_string(),
            ..delivery.clone()
        };
        let first = r#"{"x-edge-request-id":"one"}"#;
        let second = r#"{"x-edge-request-id":"two"}"#;
        let later = at() + Duration::from_nanos(1);
        let fields = [
            ("the record itself", unique_field(&delivery, at(), first)),
            ("another record of the same instant", unique_field(&delivery, at(), second)),
            ("the same record a nanosecond later", unique_field(&delivery, later, first)),
            (
                "the same record, carried by another delivery",
                unique_field(&sibling, at(), first),
            ),
        ];
        for (index, (case, field)) in fields.iter().enumerate() {
            for (other_case, other) in fields.iter().skip(index + 1) {
                assert_ne!(
                    field, other,
                    "case {case} against {other_case}: expected distinct fields, got {field:?} \
                     twice"
                );
            }
        }
        assert_eq!(
            unique_field(&delivery, at(), first),
            fields[0].1,
            "case the same record twice: expected the same field, got {:?} and {:?}",
            unique_field(&delivery, at(), first),
            fields[0].1
        );
    }

    /// RULE: a log-group delivery of the general reading is the same delivery, in the view a
    /// sender that delivers into log groups alone reads; a delivery that writes elsewhere is
    /// not in that view at all.
    #[test]
    fn only_a_log_group_delivery_appears_in_the_log_group_view() {
        for (case, destination, expected) in [
            (
                "a log group",
                VendedLogDestination::LogGroup {
                    log_group: "/vended/one".to_string(),
                },
                Some("/vended/one"),
            ),
            (
                "a bucket",
                VendedLogDestination::Bucket {
                    bucket: "vended-logs".to_string(),
                    prefix: String::new(),
                    suffix_path: String::new(),
                    hive_compatible_path: false,
                },
                None,
            ),
            (
                "a delivery stream",
                VendedLogDestination::DeliveryStream {
                    delivery_stream: "vended".to_string(),
                },
                None,
            ),
        ] {
            let view = carried_to(destination).into_log_group_delivery();
            let got = view.as_ref().map(|view| view.log_group.as_str());
            assert_eq!(
                got, expected,
                "case {case}: expected {expected:?}, got {got:?}"
            );
        }
    }
}
