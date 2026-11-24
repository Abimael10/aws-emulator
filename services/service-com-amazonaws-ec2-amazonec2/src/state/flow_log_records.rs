// SPDX-License-Identifier: CC0-1.0

//! What ONE flow log RECORD is: the fields it may carry, the `LogFormat` that lays them out,
//! and the line a published record is written as.
//!
//! "Flow log data for a monitored network interface is recorded as flow log records, which
//! are log events consisting of fields that describe the traffic flow" (`CreateFlowLogs`),
//! and which fields a record carries — and in which order — is the flow log's own
//! `LogFormat`. So the vocabulary of fields, the reading of a format's `${field-id}` tokens
//! and the rendering of one record against that format are one concern, held here: the
//! create that VALIDATES a format and the delivery that WRITES records under it
//! ([`super::flow_log_delivery`]) read the same catalog, so a format a create accepts is a
//! format the delivery can write.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The record format a flow log created without a `LogFormat` gets: "If you omit this
/// parameter, the flow log is created using the default format"
/// (`CreateFlowLogsRequest$LogFormat`), which is the default set of fields AWS documents
/// for a flow log record, in the order it documents them (Amazon VPC User Guide, "Flow log
/// records" — the default format's field list).
///
/// A CONCRETE value rather than an absent member, because a client reads `LogFormat` to
/// learn the layout of the records it is about to parse.
pub const DEFAULT_FLOW_LOG_FORMAT: &str = "${version} ${account-id} ${interface-id} \
                                           ${srcaddr} ${dstaddr} ${srcport} ${dstport} \
                                           ${protocol} ${packets} ${bytes} ${start} \
                                           ${end} ${action} ${log-status}";

/// The field ids a flow log record may name — the vocabulary a `LogFormat` draws its
/// `${field-id}` tokens from.
///
/// "For more information about the available fields, see Flow log records in the Amazon VPC
/// User Guide or Transit Gateway Flow Log records in the Amazon Web Services Transit Gateway
/// Guide" (`CreateFlowLogsRequest$LogFormat`): the vocabulary is published PROSE, not a
/// model shape — the member is a plain `String` — so it is written out here, as the union of
/// the two published tables. The first block is the Amazon VPC User Guide's available
/// fields, in the order it lists them (the first fourteen are also the default format,
/// [`DEFAULT_FLOW_LOG_FORMAT`]); the second is the fields only a transit gateway record has,
/// from the Transit Gateway Guide's table.
///
/// One catalog for both, because `LogFormat` is validated the same way whatever resource
/// type the create monitors.
pub const FLOW_LOG_RECORD_FIELDS: [&str; 58] = [
    // Amazon VPC User Guide, "Flow log records" — available fields.
    "version",
    "account-id",
    "interface-id",
    "srcaddr",
    "dstaddr",
    "srcport",
    "dstport",
    "protocol",
    "packets",
    "bytes",
    "start",
    "end",
    "action",
    "log-status",
    "vpc-id",
    "subnet-id",
    "instance-id",
    "tcp-flags",
    "type",
    "pkt-srcaddr",
    "pkt-dstaddr",
    "region",
    "az-id",
    "sublocation-type",
    "sublocation-id",
    "pkt-src-aws-service",
    "pkt-dst-aws-service",
    "flow-direction",
    "traffic-path",
    "ecs-cluster-arn",
    "ecs-cluster-name",
    "ecs-container-instance-arn",
    "ecs-container-instance-id",
    "ecs-container-id",
    "ecs-second-container-id",
    "ecs-service-name",
    "ecs-task-definition-arn",
    "ecs-task-arn",
    "ecs-task-id",
    "reject-reason",
    // Amazon Web Services Transit Gateway Guide, "Flow log records" — the fields a transit
    // gateway record adds.
    "resource-type",
    "tgw-id",
    "tgw-attachment-id",
    "tgw-src-vpc-account-id",
    "tgw-dst-vpc-account-id",
    "tgw-src-vpc-id",
    "tgw-dst-vpc-id",
    "tgw-src-subnet-id",
    "tgw-dst-subnet-id",
    "tgw-src-eni",
    "tgw-dst-eni",
    "tgw-src-az-id",
    "tgw-dst-az-id",
    "tgw-pair-details",
    "packets-lost-no-route",
    "packets-lost-blackhole",
    "packets-lost-mtu-exceeded",
    "packets-lost-ttl-expired",
];

/// What separates a field id from the tag key it reads, in a record field that reports a tag
/// of the monitored resource ("The tag configuration associated with the Flow Logs Amazon
/// EC2 Tags feature fields in your custom log format" —
/// `CreateFlowLogsRequest$TagFieldSpecifications`).
///
/// A tag field's token ends in a tag KEY the caller chose, so no published table can
/// enumerate those tokens and [`FLOW_LOG_RECORD_FIELDS`] does not try: a token carrying this
/// separator names a tag rather than a field of the record, and is read as one.
const FLOW_LOG_TAG_FIELD_SEPARATOR: char = ':';

/// The `${field-id}` tokens of `log_format`, in the order the format names them.
///
/// "Specify the fields using the `${field-id}` format, separated by spaces"
/// (`CreateFlowLogsRequest$LogFormat`): a token is what one `${` and the next `}` enclose,
/// and the text between two tokens is separator rather than a field. An unterminated `${`
/// opens no token, so nothing after it is read as one.
pub fn flow_log_format_fields(log_format: &str) -> Vec<&str> {
    let mut fields: Vec<&str> = Vec::new();
    let mut rest = log_format;
    while let Some((_, opened)) = rest.split_once("${") {
        let Some((field, tail)) = opened.split_once('}') else {
            break;
        };
        fields.push(field);
        rest = tail;
    }
    fields
}

/// The `${field-id}` tokens of `log_format` that name no field a flow log record has, each
/// listed once and in the order the format names them.
///
/// A token is recognised when it names an entry of [`FLOW_LOG_RECORD_FIELDS`], or when it
/// reads a tag of the monitored resource ([`FLOW_LOG_TAG_FIELD_SEPARATOR`], whose tag key is
/// the caller's and so is not in any published table). Every other token names a field that
/// does not exist, and the create that carries it is refused with
/// [`unknown_flow_log_fields`].
pub fn unknown_flow_log_format_fields(log_format: &str) -> Vec<&str> {
    let mut unknown: Vec<&str> = Vec::new();
    for field in flow_log_format_fields(log_format) {
        let recognised = FLOW_LOG_RECORD_FIELDS.contains(&field)
            || field.contains(FLOW_LOG_TAG_FIELD_SEPARATOR);
        if !recognised && !unknown.contains(&field) {
            unknown.push(field);
        }
    }
    unknown
}

/// Refuse a `CreateFlowLogs` whose `LogFormat` names fields a flow log record does not have:
/// HTTP 400 [`INVALID_PARAMETER`].
///
/// The format is a layout of PUBLISHED fields — "For more information about the available
/// fields, see Flow log records" (`CreateFlowLogsRequest$LogFormat`) — so a token outside
/// that vocabulary names nothing the service could write, and the whole call is refused
/// before any flow log is stored rather than one being created that could never deliver the
/// records it promises.
///
/// The message names every unrecognised token, each quoted and spelled as the request
/// spelled it, in SORTED order rather than the order the format named them — the order a
/// recorded refusal of this class pins, and the one that makes the sentence a function of
/// the SET of unrecognised tokens: two formats naming the same fields in different orders
/// are refused the same way. `unknown` is never empty, because a format whose every token is
/// a field is not refused at all.
pub fn unknown_flow_log_fields<E>(unknown: &[&str]) -> Scaffolded<E> {
    let mut sorted: Vec<&str> = unknown.to_vec();
    sorted.sort_unstable();
    Scaffolded::service_error(
        INVALID_PARAMETER,
        400,
        format!(
            "Unknown fields provided: {fields}",
            fields = sorted
                .iter()
                .map(|field| format!("'{field}'"))
                .collect::<Vec<String>>()
                .join(", "),
        ),
    )
}

/// What a record writes for a field it has no value for: "If a field is not applicable or
/// could not be computed for a specific record, the record displays a '-' symbol for that
/// entry" (Amazon VPC User Guide, "Flow log records").
///
/// So a field is never OMITTED and never filled with a stand-in value: the layout the
/// `LogFormat` states is written in full, and a field this record has nothing for says so.
pub const FLOW_LOG_FIELD_WITHOUT_A_VALUE: &str = "-";

/// `log-status` for a record that reports traffic: "OK — Data is logging normally to the
/// chosen destinations" (Amazon VPC User Guide, "Flow log records" — the `log-status` field).
pub const FLOW_LOG_STATUS_OK: &str = "OK";

/// `log-status` for the record an interval with no traffic is published as: "NODATA — There
/// was no network traffic to or from the network interface during the aggregation interval"
/// (same field).
pub const FLOW_LOG_STATUS_NODATA: &str = "NODATA";

/// The `action` of a flow the VPC's filters admitted, and of one they did not: "ACCEPT — The
/// recorded traffic was permitted by the security groups and network ACLs. REJECT — The
/// recorded traffic was not permitted by the security groups or network ACLs" (same table,
/// the `action` field). These are also the two `TrafficType` members that select on it.
pub const FLOW_LOG_ACTION_ACCEPT: &str = "ACCEPT";
pub const FLOW_LOG_ACTION_REJECT: &str = "REJECT";

/// The first index of each successive flow-log VERSION in [`FLOW_LOG_RECORD_FIELDS`], and the
/// version the fields from there on were introduced in.
///
/// "If you use the default format, the version is 2. If you use a custom format, the version
/// is the highest version among the specified fields" (Amazon VPC User Guide, "Flow log
/// records" — the `version` field), and the version each field belongs to is the version
/// column of that same table. [`FLOW_LOG_RECORD_FIELDS`] is written in the table's own order,
/// so the version of a field is decided by where it sits in it: the first fourteen are the
/// default format's (version 2), and each block after them is the block one later version
/// added.
const FLOW_LOG_FIELD_VERSION_BLOCKS: [(usize, u32); 6] =
    [(0, 2), (14, 3), (21, 4), (25, 5), (29, 6), (39, 7)];

/// The flow-log version a record written under `log_format` carries: the highest version
/// among the fields the format names ([`FLOW_LOG_FIELD_VERSION_BLOCKS`]), which for the
/// default format is 2.
///
/// A token naming no published field — the tag fields, whose key is the caller's — belongs to
/// no version block and so raises nothing.
#[must_use]
pub fn flow_log_format_version(log_format: &str) -> u32 {
    let mut version = FLOW_LOG_FIELD_VERSION_BLOCKS[0].1;
    for field in flow_log_format_fields(log_format) {
        let Some(at) = FLOW_LOG_RECORD_FIELDS
            .iter()
            .position(|published| *published == field)
        else {
            continue;
        };
        let introduced = FLOW_LOG_FIELD_VERSION_BLOCKS
            .iter()
            .rev()
            .find(|(first, _)| at >= *first)
            .map_or(version, |(_, introduced)| *introduced);
        version = version.max(introduced);
    }
    version
}

/// ONE flow of IP traffic this emulator OBSERVED on a network interface, as the fields of a
/// flow log record report it.
///
/// "Flow log data for a monitored network interface is recorded as flow log records, which
/// are log events consisting of fields that describe the traffic flow" (`CreateFlowLogs`), and
/// one record covers one flow — one 5-tuple — within the aggregation interval. Every member
/// is what the emulator OBSERVED of that flow; a quantity it does not hold is `None`, and the
/// record then writes [`FLOW_LOG_FIELD_WITHOUT_A_VALUE`] for that field rather than a number
/// nothing counted.
#[derive(Debug, Clone)]
pub struct ObservedFlow {
    /// "The source address for incoming traffic, or the IPv4 or IPv6 address of the network
    /// interface for outgoing traffic" (the `srcaddr` field).
    pub source_address: String,
    /// "The destination address for outgoing traffic, or the IPv4 or IPv6 address of the
    /// network interface for incoming traffic" (the `dstaddr` field).
    pub destination_address: String,
    /// "The source port of the traffic" and "The destination port of the traffic"
    /// (`srcport`, `dstport`), absent for a protocol that has no ports.
    pub source_port: Option<i32>,
    pub destination_port: Option<i32>,
    /// "The IANA protocol number of the traffic" (the `protocol` field).
    pub protocol: i32,
    /// "The number of packets transferred during the flow" and "The number of bytes
    /// transferred during the flow" (`packets`, `bytes`).
    pub packets: Option<u64>,
    pub bytes: Option<u64>,
    /// "The type of traffic. The possible values are: IPv4 | IPv6 | EFA" (the `type` field).
    pub traffic_family: String,
    /// What the VPC's filters did with the flow: [`FLOW_LOG_ACTION_ACCEPT`] or
    /// [`FLOW_LOG_ACTION_REJECT`].
    pub action: String,
    /// "The direction of the flow with respect to the interface where traffic is captured"
    /// (the `flow-direction` field): `ingress` or `egress`.
    pub direction: String,
}

/// The `flow-direction` of traffic INTO the interface the record is captured on, and of
/// traffic out of it (the `flow-direction` field's two values).
pub const FLOW_DIRECTION_INGRESS: &str = "ingress";
pub const FLOW_DIRECTION_EGRESS: &str = "egress";

/// Everything ONE record reports, out of which the fields its `LogFormat` names are written.
///
/// The record is about ONE interface in ONE aggregation window, so the interface's own facts
/// and the window's bounds are always known; the FLOW is what an interval with no traffic has
/// none of ([`FLOW_LOG_STATUS_NODATA`]).
#[derive(Debug, Clone, Copy)]
pub struct FlowLogRecordFacts<'a> {
    /// The version the record is written in ([`flow_log_format_version`]).
    pub version: u32,
    /// "The Amazon Web Services account ID of the owner of the source network interface"
    /// (the `account-id` field).
    pub account_id: &'a str,
    /// "The Region that contains the network interface for which traffic is recorded" (the
    /// `region` field).
    pub region: &'a str,
    /// The interface the record is about.
    pub interface: &'a MonitoredInterface,
    /// "The time, in Unix seconds, of the start of the aggregation interval" and "of the end
    /// of the aggregation interval" (`start`, `end`).
    pub window_start_secs: i64,
    pub window_end_secs: i64,
    /// The flow this record reports, or `None` for the record an interval with no traffic is
    /// published as.
    pub flow: Option<&'a ObservedFlow>,
}

impl FlowLogRecordFacts<'_> {
    /// The `log-status` of this record: [`FLOW_LOG_STATUS_OK`] for a record reporting a flow,
    /// and [`FLOW_LOG_STATUS_NODATA`] for the one published for an interval with no traffic.
    #[must_use]
    pub fn log_status(&self) -> &'static str {
        if self.flow.is_some() {
            FLOW_LOG_STATUS_OK
        } else {
            FLOW_LOG_STATUS_NODATA
        }
    }

    /// What this record writes for the field `field`, and
    /// [`FLOW_LOG_FIELD_WITHOUT_A_VALUE`] for a field it has no value for.
    ///
    /// The fields of the FLOW are the ones an interval with no traffic has none of. The fields
    /// that describe the INTERFACE the record is about — its VPC, its subnet, the instance it
    /// is attached to, the Region and zone it sits in — are facts of that interface, so they
    /// are written whether or not the window carried traffic. The sublocation and Amazon ECS
    /// fields are each documented as populated only for a record of traffic in a sublocation
    /// of a Region (a Local Zone, a Wavelength Zone, an Outpost) or in an Amazon ECS task's
    /// own interface, and a transit gateway record's fields belong to a transit gateway flow
    /// log, so none of them has a value here.
    fn value_of(&self, field: &str) -> String {
        let missing = || FLOW_LOG_FIELD_WITHOUT_A_VALUE.to_string();
        let flow_text = |read: fn(&ObservedFlow) -> String| self.flow.map_or_else(missing, read);
        match field {
            "version" => self.version.to_string(),
            "account-id" => self.account_id.to_string(),
            "interface-id" => self.interface.network_interface_id.clone(),
            "vpc-id" => self.interface.vpc_id.clone().unwrap_or_else(missing),
            "subnet-id" => self.interface.subnet_id.clone().unwrap_or_else(missing),
            "instance-id" => self.interface.instance_id.clone().unwrap_or_else(missing),
            "region" => self.region.to_string(),
            "az-id" => self
                .interface
                .availability_zone_id
                .clone()
                .unwrap_or_else(missing),
            "srcaddr" => flow_text(|flow| flow.source_address.clone()),
            "dstaddr" => flow_text(|flow| flow.destination_address.clone()),
            // "The packet-level (original) source IP address of the traffic"
            // (`pkt-srcaddr`): the emulator observes one hop, so the packet-level address of
            // a flow it saw is that flow's own address.
            "pkt-srcaddr" => flow_text(|flow| flow.source_address.clone()),
            "pkt-dstaddr" => flow_text(|flow| flow.destination_address.clone()),
            "srcport" => self
                .flow
                .and_then(|flow| flow.source_port)
                .map_or_else(missing, |port| port.to_string()),
            "dstport" => self
                .flow
                .and_then(|flow| flow.destination_port)
                .map_or_else(missing, |port| port.to_string()),
            "protocol" => flow_text(|flow| flow.protocol.to_string()),
            "packets" => self
                .flow
                .and_then(|flow| flow.packets)
                .map_or_else(missing, |packets| packets.to_string()),
            "bytes" => self
                .flow
                .and_then(|flow| flow.bytes)
                .map_or_else(missing, |bytes| bytes.to_string()),
            "start" => self.window_start_secs.to_string(),
            "end" => self.window_end_secs.to_string(),
            "action" => flow_text(|flow| flow.action.clone()),
            "log-status" => self.log_status().to_string(),
            "type" => flow_text(|flow| flow.traffic_family.clone()),
            "flow-direction" => flow_text(|flow| flow.direction.clone()),
            // A field whose token names a TAG of the resource the record is about
            // ("The tag configuration associated with the Flow Logs Amazon EC2 Tags feature
            // fields in your custom log format", `CreateFlowLogsRequest$TagFieldSpecifications`):
            // the key is the part after the last separator, and the value is the one that
            // interface carries for it. The value is the one piece of a record a CALLER chose
            // the text of, so it is written only where it is a value a record can carry
            // ([`value_fits_one_field`]).
            tag_field if tag_field.contains(FLOW_LOG_TAG_FIELD_SEPARATOR) => tag_field
                .rsplit(FLOW_LOG_TAG_FIELD_SEPARATOR)
                .next()
                .and_then(|key| self.interface.tag_value(key))
                .filter(|value| value_fits_one_field(value))
                .unwrap_or_else(missing),
            // Every other published field: a quantity this emulator holds nothing of for the
            // flow, or one documented as populated only for a record of another kind.
            _ => missing(),
        }
    }
}

/// Whether `value` is a value ONE field of a record can carry: a record is one log event laid
/// out as "the fields … separated by spaces" (`CreateFlowLogsRequest$LogFormat`), so a value
/// carrying whitespace would be read as more than one field, and one carrying a line break as
/// more than one record.
///
/// The one piece of a record whose text a CALLER chooses is the value of a tag field, so it is
/// the one value this is asked of: a tag whose value does not fit a field is written as
/// [`FLOW_LOG_FIELD_WITHOUT_A_VALUE`], the same as any other field this record has no value
/// for, rather than as text that re-frames the record around it.
fn value_fits_one_field(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

/// The LINE one record is published as: the text of `log_format` with each `${field-id}`
/// token replaced by what this record reports for it.
///
/// "Specify the fields using the `${field-id}` format, separated by spaces"
/// (`CreateFlowLogsRequest$LogFormat`) — the separators are the format's own text, so a
/// format that separates its fields with anything else writes records separated that way, and
/// the FIELD ORDER is the order the format names them in. An unterminated `${` opens no token
/// ([`flow_log_format_fields`]), so what follows it is literal text.
#[must_use]
pub fn flow_log_record_line(log_format: &str, facts: &FlowLogRecordFacts<'_>) -> String {
    let mut line = String::with_capacity(log_format.len());
    let mut rest = log_format;
    while let Some((literal, opened)) = rest.split_once("${") {
        line.push_str(literal);
        let Some((field, tail)) = opened.split_once('}') else {
            line.push_str("${");
            line.push_str(opened);
            return line;
        };
        line.push_str(&facts.value_of(field));
        rest = tail;
    }
    line.push_str(rest);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account every record below is of, and the Region its interface sits in.
    const ACCOUNT: &str = "000000000000";
    const REGION: &str = "us-east-1";

    /// The interface the records below are about, carrying one tag whose value fits a field
    /// and one whose value does not.
    fn interface() -> MonitoredInterface {
        MonitoredInterface {
            network_interface_id: "eni-0123456789abcdef0".to_string(),
            vpc_id: Some("vpc-0123456789abcdef0".to_string()),
            subnet_id: Some("subnet-0123456789abcdef0".to_string()),
            instance_id: Some("i-0123456789abcdef0".to_string()),
            availability_zone_id: Some("use1-az1".to_string()),
            tags: vec![
                Tag {
                    key: Some("Name".to_string()),
                    value: Some("web-1".to_string()),
                },
                Tag {
                    key: Some("Owner".to_string()),
                    value: Some("the platform team".to_string()),
                },
            ],
        }
    }

    /// One flow of that interface, as this emulator would report one it observed.
    fn flow() -> ObservedFlow {
        ObservedFlow {
            source_address: "10.0.1.5".to_string(),
            destination_address: "10.0.2.9".to_string(),
            source_port: Some(49_152),
            destination_port: Some(80),
            protocol: 6,
            packets: Some(14),
            bytes: Some(2_048),
            traffic_family: "IPv4".to_string(),
            action: FLOW_LOG_ACTION_ACCEPT.to_string(),
            direction: FLOW_DIRECTION_EGRESS.to_string(),
        }
    }

    /// The facts of one record: `flow` for a window that carried traffic, `None` for one that
    /// carried none.
    fn facts<'a>(
        held: &'a MonitoredInterface,
        version: u32,
        flow: Option<&'a ObservedFlow>,
    ) -> FlowLogRecordFacts<'a> {
        FlowLogRecordFacts {
            version,
            account_id: ACCOUNT,
            region: REGION,
            interface: held,
            window_start_secs: 1_431_280_876,
            window_end_secs: 1_431_280_934,
            flow,
        }
    }

    /// A record is the format's own text with each `${field-id}` replaced by the value the
    /// record reports for it — so the FIELDS and their ORDER, and the separators between them,
    /// are the format's.
    #[test]
    fn a_record_is_the_format_with_each_field_replaced_by_its_value() {
        let held = interface();
        let observed = flow();
        // (case, the format, the flow the window carried, the line it is published as)
        let cases: [(&str, &str, Option<&ObservedFlow>, &str); 7] = [
            (
                "the documented default format, for a window with no traffic",
                DEFAULT_FLOW_LOG_FORMAT,
                None,
                "2 000000000000 eni-0123456789abcdef0 - - - - - - - 1431280876 1431280934 - \
                 NODATA",
            ),
            (
                "the documented default format, for a window that carried one flow",
                DEFAULT_FLOW_LOG_FORMAT,
                Some(&observed),
                "2 000000000000 eni-0123456789abcdef0 10.0.1.5 10.0.2.9 49152 80 6 14 2048 \
                 1431280876 1431280934 ACCEPT OK",
            ),
            (
                "the fields of the interface the record is about",
                "${interface-id} ${vpc-id} ${subnet-id} ${instance-id} ${az-id} ${region}",
                None,
                "eni-0123456789abcdef0 vpc-0123456789abcdef0 subnet-0123456789abcdef0 \
                 i-0123456789abcdef0 use1-az1 us-east-1",
            ),
            (
                "a separator of the format's own choosing",
                "${interface-id},${action},${log-status}",
                Some(&observed),
                "eni-0123456789abcdef0,ACCEPT,OK",
            ),
            (
                "a tag of the resource the record is about",
                "${interface-id} ${aws:ec2:instance:tag:Name}",
                None,
                "eni-0123456789abcdef0 web-1",
            ),
            (
                "a tag whose value would not fit one field, and one the resource lacks",
                "${aws:ec2:instance:tag:Owner} ${aws:ec2:instance:tag:Absent}",
                None,
                "- -",
            ),
            (
                "an unterminated field marker, which opens no field at all",
                "${interface-id} ${log-status} ${unclosed",
                None,
                "eni-0123456789abcdef0 NODATA ${unclosed",
            ),
        ];
        for (case, log_format, carried, expected) in cases {
            let line = flow_log_record_line(log_format, &facts(&held, 2, carried));
            assert_eq!(
                line, expected,
                "case {case:?}: expected the record {expected:?}, got {line:?}"
            );
        }
    }

    /// The version a record carries is the highest version among the fields its format names,
    /// which for the default format is 2.
    #[test]
    fn the_version_is_the_highest_among_the_fields_the_format_names() {
        // (case, the format, the version a record of it carries)
        let cases: [(&str, &str, u32); 7] = [
            ("the documented default format", DEFAULT_FLOW_LOG_FORMAT, 2),
            ("one field of the default format", "${action}", 2),
            ("a field of the next published block", "${version} ${vpc-id}", 3),
            ("the last field of that block", "${pkt-dstaddr}", 3),
            ("a field of the block after it", "${az-id} ${srcaddr}", 4),
            ("a field of the block after that", "${flow-direction}", 5),
            (
                "a tag field, which belongs to no published block",
                "${aws:ec2:instance:tag:Name}",
                2,
            ),
        ];
        for (case, log_format, expected) in cases {
            let version = flow_log_format_version(log_format);
            assert_eq!(
                version, expected,
                "case {case:?}: expected version {expected}, got {version}"
            );
        }
    }

    /// Whether a value fits ONE field of a record: a record is one log event whose fields the
    /// format separates, so a value carrying whitespace does not.
    #[test]
    fn a_value_fits_one_field_only_when_it_carries_no_whitespace() {
        // (case, the value, whether a field can carry it)
        let cases: [(&str, &str, bool); 6] = [
            ("a plain value", "web-1", true),
            ("a value carrying punctuation a tag may hold", "team/web:1", true),
            ("a value carrying a space", "the platform team", false),
            ("a value carrying a line break", "web-1\nweb-2", false),
            ("a value carrying a tab", "web-1\tweb-2", false),
            ("no value at all", "", false),
        ];
        for (case, value, fits) in cases {
            assert_eq!(
                value_fits_one_field(value),
                fits,
                "case {case:?}: expected fits={fits}, got {}",
                value_fits_one_field(value)
            );
        }
    }
}
