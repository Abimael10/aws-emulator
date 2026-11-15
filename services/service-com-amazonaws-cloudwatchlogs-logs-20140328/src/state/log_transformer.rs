// SPDX-License-Identifier: CC0-1.0

//! The AWS-VENDED log record formats a log group's transformer reads: the published layout of
//! one vended log type, and the fields one record of it carries.
//!
//! A vended parser of [`super::transformer`] "parse[s] Amazon VPC vended logs, extract[s]
//! fields, and convert[s] them into a JSON format" (`ParseVPC`) — it reads the WHOLE log event
//! message as a record of its own log type ("This processor always processes the entire log
//! event message") and answers the fields that record carries, each already in the JSON type
//! the format gives it. This module holds those formats, apart from the pipeline that applies
//! them, because a format is a published property of the log type rather than of any
//! processor's configuration.
//!
//! Two formats are held here: the VPC flow log record, which `parseVPC` reads, and the WAF
//! vended record, which `parseWAF` reads. The other vended parsers (`parseCloudfront`,
//! `parseRoute53`, `parsePostgres`) read one record layout each as well, and gain a reading
//! here when that layout is written down; until then [`super::transformer`] leaves the event
//! as it found it for them rather than inventing fields nothing derived.
//!
//! Reading one record costs one pass over the message: for the flow log record the fields are
//! the message's space-delimited fields ([`super::filter_pattern::space_delimited_fields`]),
//! paired with the catalogue of the format, which bounds how many fields one record can
//! produce whatever the message carries; for the WAF record, whose own form is JSON, they are
//! the members the document carries, read once.

use ::serde_json::{Map, Value};

/// The value a vended record carries for a field that "is not applicable or could not be
/// computed for a specific record": the record "displays a '-' symbol for that entry". Such a
/// field names no observation, so it is left OUT of the parsed fields rather than carrying
/// the placeholder.
const UNAVAILABLE_FIELD: &str = "-";

/// The JSON type a parsed field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldType {
    /// A count, a port, a protocol number or an instant: a JSON number.
    Number,
    /// An identifier, an address or a status word: a JSON string.
    Text,
}

impl FieldType {
    /// One field's value in this type. A field typed as a number whose text is not one keeps
    /// the text the record carried: the record said something, and dropping it would report
    /// less than the record holds.
    fn value(self, text: &str) -> Value {
        match self {
            Self::Number => text
                .parse::<i64>()
                .map_or_else(|_| Value::String(text.to_string()), Value::from),
            Self::Text => Value::String(text.to_string()),
        }
    }
}

/// The fields of a VPC flow log record in the DEFAULT format, in the order the record carries
/// them, each with the JSON type the parsed field takes.
///
/// Source: the flow log record's default format — "version account-id interface-id srcaddr
/// dstaddr srcport dstport protocol packets bytes start end action log-status" — with each
/// field spelled in the camelCase `parseVPC` converts it to. That processor "doesn't support
/// custom log formats", so this is the whole catalogue: a record carrying more fields than it
/// names leaves them unparsed, and one carrying fewer parses as far as it goes.
const VPC_FLOW_LOG_FIELDS: [(&str, FieldType); 14] = [
    ("version", FieldType::Number),
    ("accountId", FieldType::Text),
    ("interfaceId", FieldType::Text),
    ("srcAddr", FieldType::Text),
    ("dstAddr", FieldType::Text),
    ("srcPort", FieldType::Number),
    ("dstPort", FieldType::Number),
    ("protocol", FieldType::Number),
    ("packets", FieldType::Number),
    ("bytes", FieldType::Number),
    ("start", FieldType::Number),
    ("end", FieldType::Number),
    ("action", FieldType::Text),
    ("logStatus", FieldType::Text),
];

/// The fields one VPC flow log record carries, in the order the format lists them: each field
/// the record supplies, under its own name and in its own JSON type, and nothing for a field
/// whose value is the "not available" placeholder.
pub fn vpc_flow_log_fields(record: &str) -> Vec<(&'static str, Value)> {
    let carried = super::filter_pattern::space_delimited_fields(record);
    VPC_FLOW_LOG_FIELDS
        .iter()
        .zip(&carried)
        .filter(|(_, value)| *value != UNAVAILABLE_FIELD)
        .map(|((name, kind), value)| (*name, kind.value(value)))
        .collect()
}

/// The member of a WAF vended record that carries the request the web ACL evaluated, under
/// which the request's headers sit.
const WAF_REQUEST_MEMBER: &str = "httpRequest";

/// The member of that request carrying the headers the request was sent with. The vended
/// record writes it as a LIST, one entry per header.
const WAF_HEADERS_MEMBER: &str = "headers";

/// The member of one entry of that list naming the header.
const WAF_HEADER_NAME_MEMBER: &str = "name";

/// The member of one entry of that list carrying what was sent under that header.
const WAF_HEADER_VALUE_MEMBER: &str = "value";

/// The fields one WAF vended record carries, "convert[ed] ... into a JSON format": the record
/// is itself a JSON document, so its own members are the fields, each under the name the
/// record gives it, with ONE conversion applied — the request's headers, which the record
/// vends as a list of entries each naming a header and carrying its value, become a single
/// object KEYED BY HEADER NAME, so a reader addressing one header by name reaches its value.
/// Every other member of the record, the request's own other members included, is the member
/// the record carried, unchanged.
///
/// Source: the published WAF log format, whose `httpRequest.headers` is the list of the
/// request's headers, each entry carrying a `name` and a `value`.
///
/// A message that is no JSON object is no record of this format and yields no field.
pub fn waf_record_fields(record: &str) -> Vec<(String, Value)> {
    let Ok(Value::Object(members)) = ::serde_json::from_str::<Value>(record) else {
        return Vec::new();
    };
    members
        .into_iter()
        .map(|(name, mut value)| {
            if name == WAF_REQUEST_MEMBER {
                key_headers_by_name(&mut value);
            }
            (name, value)
        })
        .collect()
}

/// Key `request`'s headers by header name, where it carries them as the list the vended record
/// writes them as: every entry that names a header and carries a value contributes one member,
/// and where two entries name one header the later value stands, because one member of an
/// object holds one value. A request whose headers are not that list — already keyed, absent,
/// or not a request structure at all — is left exactly as the record carried it.
///
/// The entries are MOVED into the keyed object rather than copied into it, so one header's
/// value is not duplicated while an event is read.
fn key_headers_by_name(request: &mut Value) {
    let Some(members) = request.as_object_mut() else {
        return;
    };
    let Some(Value::Array(entries)) = members.get_mut(WAF_HEADERS_MEMBER) else {
        return;
    };
    let entries = ::std::mem::take(entries);
    let mut keyed = Map::new();
    for entry in entries {
        let Value::Object(mut entry) = entry else {
            continue;
        };
        let Some(Value::String(name)) = entry.remove(WAF_HEADER_NAME_MEMBER) else {
            continue;
        };
        let Some(value) = entry.remove(WAF_HEADER_VALUE_MEMBER) else {
            continue;
        };
        keyed.insert(name, value);
    }
    members.insert(WAF_HEADERS_MEMBER.to_string(), Value::Object(keyed));
}

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::{vpc_flow_log_fields, waf_record_fields};

    /// A record parses to the catalogue's fields with its numeric fields as JSON numbers and
    /// its identifiers as JSON strings, and a field the record could not compute is ABSENT
    /// rather than carrying the placeholder.
    #[test]
    fn a_flow_log_record_parses_to_typed_fields_without_its_unavailable_ones() {
        let cases: [(&str, &str, &[&str], &[&str]); 3] = [
            (
                "a full record",
                "2 123456789012 eni-1 10.0.0.1 10.0.0.2 443 49152 6 10 840 1700000000 \
                 1700000060 ACCEPT OK",
                &[
                    "version", "accountId", "interfaceId", "srcAddr", "dstAddr", "srcPort",
                    "dstPort", "protocol", "packets", "bytes", "start", "end", "action",
                    "logStatus",
                ],
                &[],
            ),
            (
                "a record that reports no data",
                "2 123456789012 eni-1 - - - - - - - 1700000000 1700000060 - NODATA",
                &["version", "accountId", "interfaceId", "start", "end", "logStatus"],
                &[
                    "srcAddr", "dstAddr", "srcPort", "dstPort", "protocol", "packets", "bytes",
                    "action",
                ],
            ),
            (
                "a record carrying fewer fields than the format names",
                "2 123456789012 eni-1",
                &["version", "accountId", "interfaceId"],
                &["srcAddr", "action", "logStatus"],
            ),
        ];
        // The fields the format types as numbers, from the catalogue's own source.
        let numeric = [
            "version", "srcPort", "dstPort", "protocol", "packets", "bytes", "start", "end",
        ];
        for (case, record, present, absent) in cases {
            let fields = vpc_flow_log_fields(record);
            let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
            assert_eq!(
                names, present,
                "case {case}: the record's own fields are parsed, in format order"
            );
            for missing in absent {
                assert!(
                    !names.contains(missing),
                    "case {case}: {missing} is absent for an unavailable value, got {names:?}"
                );
            }
            for (name, value) in &fields {
                let expected_number = numeric.contains(name);
                assert_eq!(
                    value.is_number(),
                    expected_number,
                    "case {case}: {name} is a JSON number: {expected_number}, got {value}"
                );
            }
        }
    }

    /// A WAF record's request headers are read as an object keyed by header name, whatever
    /// the entries the list carries, and every other member of the record is the member the
    /// record carried.
    #[test]
    fn a_waf_record_reads_its_request_headers_keyed_by_header_name() {
        let cases: [(&str, ::serde_json::Value, ::serde_json::Value); 6] = [
            (
                "several headers",
                json!([
                    {"name": "Host", "value": "a.example"},
                    {"name": "Accept", "value": "*/*"},
                ]),
                json!({"Host": "a.example", "Accept": "*/*"}),
            ),
            (
                "one header",
                json!([{"name": "Host", "value": "a.example"}]),
                json!({"Host": "a.example"}),
            ),
            ("no header at all", json!([]), json!({})),
            (
                "two entries naming one header",
                json!([
                    {"name": "Cookie", "value": "a=1"},
                    {"name": "Cookie", "value": "b=2"},
                ]),
                json!({"Cookie": "b=2"}),
            ),
            (
                "an entry naming no header",
                json!([{"value": "a.example"}, {"name": "Accept", "value": "*/*"}]),
                json!({"Accept": "*/*"}),
            ),
            (
                // Already keyed: nothing to convert, so the member stands.
                "headers already keyed by name",
                json!({"Host": "a.example"}),
                json!({"Host": "a.example"}),
            ),
        ];
        for (case, headers, expected) in cases {
            let record = json!({
                "formatVersion": 1,
                "action": "ALLOW",
                "httpRequest": {"clientIp": "192.0.2.10", "headers": headers, "uri": "/a"},
            });
            let fields: ::serde_json::Map<String, ::serde_json::Value> =
                waf_record_fields(&record.to_string()).into_iter().collect();
            assert_eq!(
                fields.get("httpRequest").and_then(|request| request.get("headers")),
                Some(&expected),
                "case {case}: the request's headers are keyed by name, got {fields:?}"
            );
            assert_eq!(
                fields.get("formatVersion"),
                Some(&json!(1)),
                "case {case}: every other member is the one the record carried, got {fields:?}"
            );
            assert_eq!(
                fields.get("httpRequest").and_then(|request| request.get("uri")),
                Some(&json!("/a")),
                "case {case}: the request's other members are unchanged, got {fields:?}"
            );
        }
    }

    /// A message that is no JSON object is no record of the WAF format, so it reads as no
    /// field and the ingested message stands.
    #[test]
    fn a_message_that_is_no_waf_record_reads_as_no_field() {
        for case in ["not json at all", "[1,2,3]", "\"text\"", ""] {
            let fields = waf_record_fields(case);
            assert!(
                fields.is_empty(),
                "case {case:?}: no field is derived, got {fields:?}"
            );
        }
    }
}
