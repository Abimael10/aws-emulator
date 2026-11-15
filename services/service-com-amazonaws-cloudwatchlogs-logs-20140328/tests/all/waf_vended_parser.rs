// SPDX-License-Identifier: CC0-1.0

//! The `parseWAF` reading of an ingested log event, end to end over the wire with the Kinesis
//! emulator mounted beside this service, so the transformed record is read where a consumer
//! reads it.
//!
//! RULE: the `parseWAF` processor reads the whole ingested message as one WAF vended log
//! record and converts it "into a JSON format" — "This processor always processes the entire
//! log event message" — which, for a record that is already a JSON document, means its own
//! members stand as the transformed event's fields with ONE conversion applied: the request's
//! headers, which the vended record carries as a LIST of entries each naming a header and
//! carrying its value, become a single object KEYED BY HEADER NAME, so a downstream key path
//! addressing one header by name reaches its value. Every other member of the record — the
//! request's own other members included — is the member the record carried, and the log event
//! the group STORED is the ingested bytes.
//!
//! The rows below differ along what the rule generalises over: how many headers the request
//! carries (several, one, none), what their values are, repetition of one header name, the
//! FORM the headers member takes (the vended list, an already-keyed object, absent), the other
//! members the record carries (including a different list of named structures, which is NOT
//! the headers list and stays a list), whether the message is a record of this format at all,
//! and whether a later processor addresses a header by name. The recorded case — a record with
//! a `Host` and a `User-Agent` header — is one row.
//!
//! The member names and the record's own members come from the published WAF log format for
//! web ACL traffic, which `ParseWAF`'s own model documentation points at: the record carries
//! `timestamp`, `formatVersion`, `webaclId`, `terminatingRuleId`, `terminatingRuleType`,
//! `action`, `httpSourceName`, `httpSourceId`, `ruleGroupList`, `rateBasedRuleList`,
//! `nonTerminatingMatchingRules`, optional `labels`, and `httpRequest` with
//! `clientIp`/`country`/`headers`/`uri`/`args`/`httpVersion`/`httpMethod`/`requestId`, where
//! each entry of `headers` carries a `name` and a `value`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::harness::base_ms;
use crate::subscription_fan_out::{role_arn, stream_arn, Emulator};

/// The log stream every case writes to, and the filter every case attaches.
const LOG_STREAM: &str = "waf-logs";
const FILTER: &str = "to-the-stream";

/// The status a Kinesis stream reports once it can take a delivery.
const STREAM_ACTIVE: &str = "ACTIVE";

/// How long the destination stream is given to report itself usable, and how often it is
/// asked. The deadline is generous: it is not a limit this suite tests, only room for a
/// loaded host, and the wait ends as soon as the stream says it is ready.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Wait until the destination stream reports itself usable — polled for, because the status
/// it leaves `CREATING` for is owed by the clock.
async fn await_active_stream(emulator: &Emulator, case: &str, stream: &str) {
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let summary = emulator
            .kinesis(case, "DescribeStreamSummary", &json!({"StreamName": stream}))
            .await;
        let status = summary["StreamDescriptionSummary"]["StreamStatus"].clone();
        if status == json!(STREAM_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the destination stream {stream} to report \
             {STREAM_ACTIVE}, got {summary}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One WAF vended record carrying `request` as its `httpRequest`, with the other members the
/// format lists.
fn waf_record(request: Value) -> Value {
    json!({
        "timestamp": 1_700_000_000_000_i64,
        "formatVersion": 1,
        "webaclId": "arn:aws:wafv2:us-east-1:123456789012:regional/webacl/the-acl/0123abcd",
        "terminatingRuleId": "Default_Action",
        "terminatingRuleType": "REGULAR",
        "action": "ALLOW",
        "httpSourceName": "ALB",
        "httpSourceId": "123456789012-app/the-alb/0123abcd",
        "ruleGroupList": [],
        "rateBasedRuleList": [],
        "nonTerminatingMatchingRules": [],
        "httpRequest": request,
    })
}

/// The request of a WAF record, carrying `headers` however they are written.
fn request_with_headers(headers: Value) -> Value {
    json!({
        "clientIp": "192.0.2.10",
        "country": "US",
        "headers": headers,
        "uri": "/a",
        "args": "q=1",
        "httpVersion": "HTTP/1.1",
        "httpMethod": "GET",
        "requestId": "r1",
    })
}

/// One entry of the vended headers list.
fn header_entry(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value})
}

/// What a row expects at the destination for the one event it writes.
enum Expected {
    /// The delivered message parses as JSON and IS this document.
    Document(Value),
    /// The delivered message is this text, byte for byte — the event as ingested.
    Text(String),
}

/// One row of the table.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// The log group the row writes to, one per row so the destination's records can be
    /// attributed back to it.
    group: &'static str,
    /// The `transformerConfig` the row puts on the group.
    transformer: Value,
    /// The one message the row ingests.
    message: String,
    /// What the destination must hold for it.
    expected: Expected,
}

/// A one-processor chain naming `parseWAF`.
fn parse_waf() -> Value {
    json!([{"parseWAF": {}}])
}

/// The table.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();

    // The recorded case: a request carrying two headers.
    rows.push(Row {
        case: "a request carrying two headers",
        group: "/aws/emulator/waf-two-headers",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!([
            header_entry("Host", "a.example"),
            header_entry("User-Agent", "sim2real/1.0"),
        ])))
        .to_string(),
        expected: Expected::Document(waf_record(request_with_headers(
            json!({"Host": "a.example", "User-Agent": "sim2real/1.0"}),
        ))),
    });

    // More headers, one of them sent empty, and one whose name repeats the casing a client
    // chose: a header name is the key whatever it spells.
    rows.push(Row {
        case: "a request carrying several headers, one of them empty",
        group: "/aws/emulator/waf-many-headers",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!([
            header_entry("host", "b.example"),
            header_entry("accept", "*/*"),
            header_entry("x-forwarded-for", ""),
            header_entry("Content-Length", "0"),
        ])))
        .to_string(),
        expected: Expected::Document(waf_record(request_with_headers(json!({
            "host": "b.example",
            "accept": "*/*",
            "x-forwarded-for": "",
            "Content-Length": "0",
        })))),
    });

    rows.push(Row {
        case: "a request carrying one header",
        group: "/aws/emulator/waf-one-header",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!([header_entry(
            "Host",
            "c.example"
        )])))
        .to_string(),
        expected: Expected::Document(waf_record(request_with_headers(
            json!({"Host": "c.example"}),
        ))),
    });

    rows.push(Row {
        case: "a request carrying no header",
        group: "/aws/emulator/waf-no-header",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!([]))).to_string(),
        expected: Expected::Document(waf_record(request_with_headers(json!({})))),
    });

    // One object member holds one value, so the later entry for a repeated name stands.
    rows.push(Row {
        case: "two entries naming one header",
        group: "/aws/emulator/waf-repeated-header",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!([
            header_entry("Cookie", "a=1"),
            header_entry("Cookie", "b=2"),
        ])))
        .to_string(),
        expected: Expected::Document(waf_record(request_with_headers(
            json!({"Cookie": "b=2"}),
        ))),
    });

    // Already keyed by name: there is no list to convert, so the member stands.
    rows.push(Row {
        case: "a request whose headers are already keyed by name",
        group: "/aws/emulator/waf-keyed-headers",
        transformer: parse_waf(),
        message: waf_record(request_with_headers(json!({"Host": "d.example"}))).to_string(),
        expected: Expected::Document(waf_record(request_with_headers(
            json!({"Host": "d.example"}),
        ))),
    });

    rows.push(Row {
        case: "a request carrying no headers member at all",
        group: "/aws/emulator/waf-headerless-request",
        transformer: parse_waf(),
        message: waf_record(json!({
            "clientIp": "192.0.2.11",
            "country": "GB",
            "uri": "/b",
            "httpMethod": "POST",
        }))
        .to_string(),
        expected: Expected::Document(waf_record(json!({
            "clientIp": "192.0.2.11",
            "country": "GB",
            "uri": "/b",
            "httpMethod": "POST",
        }))),
    });

    // A record carrying the format's other named structures: the labels a rule attached and a
    // rule group's own matches are lists of named structures too, and neither is the request's
    // headers list, so both stand as the record wrote them.
    let labelled_request = request_with_headers(json!([header_entry("Host", "e.example")]));
    let other_members = |headers: Value| {
        let mut record = waf_record(headers);
        let members = record.as_object_mut().expect("the record is an object");
        members.insert(
            "labels".to_string(),
            json!([{"name": "awswaf:managed:aws:bot-control:bot:category:monitoring"}]),
        );
        members.insert(
            "ruleGroupList".to_string(),
            json!([{
                "ruleGroupId": "AWS#AWSManagedRulesCommonRuleSet",
                "terminatingRule": null,
                "nonTerminatingMatchingRules": [],
                "excludedRules": null,
            }]),
        );
        members.insert("responseCodeSent".to_string(), json!(403));
        record
    };
    rows.push(Row {
        case: "a record carrying the format's other named structures",
        group: "/aws/emulator/waf-other-members",
        transformer: parse_waf(),
        message: other_members(labelled_request).to_string(),
        expected: Expected::Document(other_members(request_with_headers(
            json!({"Host": "e.example"}),
        ))),
    });

    // A later processor addressing one header BY NAME: the conversion is what such a key path
    // reads, which is the whole reason to run this parser rather than a plain JSON one.
    rows.push(Row {
        case: "a header addressed by name by a later processor",
        group: "/aws/emulator/waf-header-by-name",
        transformer: json!([
            {"parseWAF": {}},
            {"copyValue": {"entries": [
                {"source": "httpRequest.headers.Host", "target": "host",
                 "overwriteIfExists": false},
            ]}},
        ]),
        message: waf_record(request_with_headers(json!([
            header_entry("Host", "f.example"),
            header_entry("User-Agent", "sim2real/1.0"),
        ])))
        .to_string(),
        expected: Expected::Document({
            let mut document = waf_record(request_with_headers(
                json!({"Host": "f.example", "User-Agent": "sim2real/1.0"}),
            ));
            document
                .as_object_mut()
                .expect("the record is an object")
                .insert("host".to_string(), json!("f.example"));
            document
        }),
    });

    // A message that is no record of this format structures nothing, so the event has no
    // transformed version and the ingested message is what the destination is handed.
    let not_a_record = "not a WAF record at all";
    rows.push(Row {
        case: "a message that is no WAF record",
        group: "/aws/emulator/waf-not-a-record",
        transformer: parse_waf(),
        message: not_a_record.to_string(),
        expected: Expected::Text(not_a_record.to_string()),
    });

    rows
}

/// Every row of the table: what the destination is handed for the one event it ingests, and
/// that the stored event is the ingested bytes whatever the group transforms.
#[tokio::test]
async fn parse_waf_keys_a_vended_records_request_headers_by_header_name() {
    let emulator = Emulator::new();
    let stream = "waf-delivery";
    emulator.create_stream("the destination stream", stream).await;
    await_active_stream(&emulator, "the destination stream", stream).await;

    let rows = rows();
    let at = base_ms();
    for row in &rows {
        let case = row.case;
        emulator
            .logs(case, "CreateLogGroup", &json!({"logGroupName": row.group}))
            .await;
        emulator
            .logs(
                case,
                "CreateLogStream",
                &json!({"logGroupName": row.group, "logStreamName": LOG_STREAM}),
            )
            .await;
        emulator
            .logs(
                case,
                "PutTransformer",
                &json!({"logGroupIdentifier": row.group,
                        "transformerConfig": row.transformer}),
            )
            .await;
        let stored = emulator
            .logs(case, "GetTransformer", &json!({"logGroupIdentifier": row.group}))
            .await;
        assert_eq!(
            stored["transformerConfig"], row.transformer,
            "case {case}: the group carries the transformer it was put, got {stored}"
        );
        emulator
            .logs(
                case,
                "PutSubscriptionFilter",
                &json!({
                    "logGroupName": row.group,
                    "filterName": FILTER,
                    "filterPattern": "",
                    "destinationArn": stream_arn(stream),
                    "roleArn": role_arn(),
                    "applyOnTransformedLogs": true,
                }),
            )
            .await;
        emulator
            .logs(
                case,
                "PutLogEvents",
                &json!({
                    "logGroupName": row.group,
                    "logStreamName": LOG_STREAM,
                    "logEvents": [{"timestamp": at, "message": row.message}],
                }),
            )
            .await;

        // A transformer is a second READING of an ingested event: what the group stored is
        // the message the batch carried, byte for byte.
        let read = emulator
            .logs(
                case,
                "GetLogEvents",
                &json!({"logGroupName": row.group, "logStreamName": LOG_STREAM}),
            )
            .await;
        let kept: Vec<String> = read["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|event| event["message"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            kept,
            vec![row.message.clone()],
            "case {case}: the stored event is the ingested one, got {kept:?}"
        );
    }

    let records = emulator.delivered("the delivered records", stream).await;
    for row in &rows {
        let case = row.case;
        let carried: Vec<String> = records
            .iter()
            .filter(|(_key, envelope)| envelope["logGroup"] == json!(row.group))
            .flat_map(|(_key, envelope)| {
                envelope["logEvents"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|event| event["message"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<String>>()
            })
            .collect();
        assert_eq!(
            carried.len(),
            1,
            "case {case}: one ingested event owes its filter one delivered message, got \
             {carried:?}"
        );
        let message = &carried[0];
        match &row.expected {
            Expected::Text(text) => assert_eq!(
                message, text,
                "case {case}: expected the event as ingested, got {message}"
            ),
            Expected::Document(document) => {
                let parsed: Value = serde_json::from_str(message).unwrap_or_else(|error| {
                    panic!(
                        "case {case}: a filter applying on transformed logs must deliver the \
                         transformed JSON event, got {error} for {message}"
                    )
                });
                assert_eq!(
                    &parsed, document,
                    "case {case}: expected the transformed record {document}, got {message}"
                );
            }
        }
    }
}
