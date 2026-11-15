// SPDX-License-Identifier: CC0-1.0

//! The buffering, retry, S3-backup-mode and append-only members a destination description
//! reports, for every destination kind BESIDE Apache Iceberg Tables.
//!
//! The rule under test: `DescribeDeliveryStream` reports a destination's EFFECTIVE
//! configuration, so a buffering, retry, S3-backup-mode or append-only member whose
//! default the service documents for that destination kind comes back carrying that
//! default when the create left it out, and carries the submitted value when the create
//! named one. Where the model documents no default for a member there is nothing to
//! materialise and the member stays absent.
//!
//! The rows differ along what the rule generalises over: the destination kind (every kind
//! the model gives a buffering, retry or backup-mode member), WHICH of those members the
//! create supplies, and — where one is supplied — its value at the published bounds of the
//! member's range (`SizeInMBs` 1..128 per kind, `IntervalInSeconds` 0..900,
//! `DurationInSeconds` 0..7200). The defaults asserted are the ones each kind's own model
//! documents, from
//! `smithy_query(query="shape",shape="BufferingHints,ElasticsearchBufferingHints,
//! AmazonopensearchserviceBufferingHints,AmazonOpenSearchServerlessBufferingHints,
//! SplunkBufferingHints,SnowflakeBufferingHints,HttpEndpointBufferingHints,RetryOptions,
//! ElasticsearchRetryOptions,AmazonopensearchserviceRetryOptions,
//! AmazonOpenSearchServerlessRetryOptions,RedshiftRetryOptions,SplunkRetryOptions,
//! HttpEndpointRetryOptions,SnowflakeRetryOptions")`.
//!
//! One suite owns each kind: the Apache Iceberg Tables rows of this rule — the four members
//! with nothing supplied, at each published bound, and half-supplied — are in
//! `iceberg_destination_delivery` (`an_iceberg_destination_reports_its_documented_defaults`)
//! and are not repeated here, and `description_defaults` owns the `S3BackupMode` half on its
//! own, so the `S3BackupMode` expectations below are only what each row needs to read the
//! whole effective configuration back.
//!
//! Every request goes through the real router, so what is asserted is the wire body a
//! client sees. The harness (`Emulator`, `s3_configuration`, the ARNs, `at`) is the
//! lifecycle suite's.

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::{
    at, s3_configuration, snowflake_private_key, Emulator, ROLE_ARN,
};

/// An Amazon Redshift cluster JDBC URL satisfying the member's modelled pattern.
const REDSHIFT_URL: &str =
    "jdbc:redshift://example-cluster.abc123.us-west-2.redshift.amazonaws.com:5439/dev";

/// One create, and what the destination description it reads back as must report.
struct Case {
    case: &'static str,
    stream: &'static str,
    config_member: &'static str,
    config: Value,
    description_member: &'static str,
    /// `(path inside the destination description, the value it must report)`. A
    /// `Value::Null` expectation means the member must be ABSENT, because the model
    /// documents no default for it — there is nothing to materialise, and no recorded
    /// response for those kinds shows the service filling the member in either.
    expected: Vec<(&'static str, Value)>,
}

/// An `ElasticsearchDestinationConfiguration` with only its required members, plus `extra`.
fn elasticsearch(extra: Value) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "DomainARN": "arn:aws:es:us-west-2:000000000000:domain/logs-domain",
        "IndexName": "logs",
        "S3Configuration": s3_configuration(),
    });
    merge(&mut config, extra);
    config
}

/// A `RedshiftDestinationConfiguration` with only its required members, plus `extra`.
fn redshift(extra: Value) -> Value {
    let mut config = json!({
        "RoleARN": ROLE_ARN,
        "ClusterJDBCURL": REDSHIFT_URL,
        "CopyCommand": { "DataTableName": "events" },
        "Username": "analytics",
        "Password": "sup3r-secret-pw",
        "S3Configuration": s3_configuration(),
    });
    merge(&mut config, extra);
    config
}

/// A `SnowflakeDestinationConfiguration` with only its required members, plus `extra`.
fn snowflake(extra: Value) -> Value {
    let mut config = json!({
        "AccountUrl": "my-account-id.snowflakecomputing.com",
        "PrivateKey": snowflake_private_key(),
        "KeyPassphrase": "passphrase",
        "User": "loader",
        "Database": "analytics",
        "Schema": "public",
        "Table": "events",
        "RoleARN": ROLE_ARN,
        "S3Configuration": s3_configuration(),
    });
    merge(&mut config, extra);
    config
}

/// Copy every member of `extra` into `config`.
fn merge(config: &mut Value, extra: Value) {
    let members = match extra.as_object() {
        Some(members) => members.clone(),
        None => return,
    };
    for (key, value) in members {
        config[key] = value;
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "an Amazon OpenSearch Service (Elasticsearch spelling) destination with \
                   no buffering or retry member",
            stream: "elasticsearch-defaults",
            config_member: "ElasticsearchDestinationConfiguration",
            config: elasticsearch(json!({})),
            description_member: "ElasticsearchDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(5)),
                ("BufferingHints/IntervalInSeconds", json!(300)),
                ("RetryOptions/DurationInSeconds", json!(300)),
                ("S3BackupMode", json!("FailedDocumentsOnly")),
            ],
        },
        Case {
            case: "an Amazon OpenSearch Service (Elasticsearch spelling) destination that \
                   supplied buffering and retry at the largest values their ranges admit",
            stream: "elasticsearch-supplied",
            config_member: "ElasticsearchDestinationConfiguration",
            config: elasticsearch(json!({
                "BufferingHints": { "SizeInMBs": 100, "IntervalInSeconds": 900 },
                "RetryOptions": { "DurationInSeconds": 7200 },
            })),
            description_member: "ElasticsearchDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(100)),
                ("BufferingHints/IntervalInSeconds", json!(900)),
                ("RetryOptions/DurationInSeconds", json!(7200)),
            ],
        },
        Case {
            case: "an Amazon OpenSearch Service destination with no buffering or retry \
                   member, whose S3BackupMode the model gives no default",
            stream: "opensearch-defaults",
            config_member: "AmazonopensearchserviceDestinationConfiguration",
            config: json!({
                "RoleARN": ROLE_ARN,
                "ClusterEndpoint": "https://search.example.com",
                "IndexName": "docs",
                "S3Configuration": s3_configuration(),
            }),
            description_member: "AmazonopensearchserviceDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(5)),
                ("BufferingHints/IntervalInSeconds", json!(300)),
                ("RetryOptions/DurationInSeconds", json!(300)),
                ("S3BackupMode", Value::Null),
            ],
        },
        Case {
            case: "a Serverless offering for Amazon OpenSearch Service destination with no \
                   buffering or retry member",
            stream: "serverless-defaults",
            config_member: "AmazonOpenSearchServerlessDestinationConfiguration",
            config: json!({
                "RoleARN": ROLE_ARN,
                "CollectionEndpoint": "https://collection.example.com",
                "IndexName": "serverless-index",
                "S3Configuration": s3_configuration(),
            }),
            description_member: "AmazonOpenSearchServerlessDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(5)),
                ("BufferingHints/IntervalInSeconds", json!(300)),
                ("RetryOptions/DurationInSeconds", json!(300)),
                ("S3BackupMode", Value::Null),
            ],
        },
        // Splunk buffers on a MINUTE by default, not the five minutes every other kind
        // documents — the default travels with the kind's own shape.
        Case {
            case: "a Splunk destination with no buffering member, whose retry window the \
                   model gives no default",
            stream: "splunk-defaults",
            config_member: "SplunkDestinationConfiguration",
            config: json!({
                "HECEndpoint": "https://splunk.example.com:8088",
                "HECEndpointType": "Event",
                "HECToken": "hec-token",
                "S3Configuration": s3_configuration(),
            }),
            description_member: "SplunkDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(5)),
                ("BufferingHints/IntervalInSeconds", json!(60)),
                ("RetryOptions", Value::Null),
                ("S3BackupMode", json!("FailedEventsOnly")),
            ],
        },
        Case {
            case: "a Splunk destination that supplied buffering at the smallest values its \
                   own ranges admit",
            stream: "splunk-supplied",
            config_member: "SplunkDestinationConfiguration",
            config: json!({
                "HECEndpoint": "https://splunk.example.com:8088",
                "HECEndpointType": "Raw",
                "HECToken": "hec-token",
                "S3Configuration": s3_configuration(),
                "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                "RetryOptions": { "DurationInSeconds": 1800 },
            }),
            description_member: "SplunkDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(1)),
                ("BufferingHints/IntervalInSeconds", json!(0)),
                ("RetryOptions/DurationInSeconds", json!(1800)),
            ],
        },
        // Snowflake buffers by the largest size of any kind and delivers immediately by
        // default.
        Case {
            case: "a Snowflake destination with no buffering member",
            stream: "snowflake-defaults",
            config_member: "SnowflakeDestinationConfiguration",
            config: snowflake(json!({})),
            description_member: "SnowflakeDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(128)),
                ("BufferingHints/IntervalInSeconds", json!(0)),
                ("RetryOptions", Value::Null),
                ("S3BackupMode", Value::Null),
            ],
        },
        Case {
            case: "a Snowflake destination that named only the buffering interval",
            stream: "snowflake-interval-only",
            config_member: "SnowflakeDestinationConfiguration",
            config: snowflake(json!({ "BufferingHints": { "IntervalInSeconds": 900 } })),
            description_member: "SnowflakeDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(128)),
                ("BufferingHints/IntervalInSeconds", json!(900)),
            ],
        },
        Case {
            case: "an HTTP endpoint destination with no buffering member, whose retry \
                   window and S3BackupMode the model gives no default",
            stream: "http-endpoint-defaults",
            config_member: "HttpEndpointDestinationConfiguration",
            config: json!({
                "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                "S3Configuration": s3_configuration(),
            }),
            description_member: "HttpEndpointDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(5)),
                ("BufferingHints/IntervalInSeconds", json!(300)),
                ("RetryOptions", Value::Null),
                ("S3BackupMode", Value::Null),
            ],
        },
        Case {
            case: "an HTTP endpoint destination that supplied buffering at the largest \
                   buffer its own range admits",
            stream: "http-endpoint-supplied",
            config_member: "HttpEndpointDestinationConfiguration",
            config: json!({
                "EndpointConfiguration": { "Url": "https://example.com/firehose" },
                "S3Configuration": s3_configuration(),
                "BufferingHints": { "SizeInMBs": 64, "IntervalInSeconds": 120 },
            }),
            description_member: "HttpEndpointDestinationDescription",
            expected: vec![
                ("BufferingHints/SizeInMBs", json!(64)),
                ("BufferingHints/IntervalInSeconds", json!(120)),
            ],
        },
        // Amazon Redshift retries for an HOUR by default, the longest window any kind
        // documents.
        Case {
            case: "an Amazon Redshift destination with no retry member",
            stream: "redshift-defaults",
            config_member: "RedshiftDestinationConfiguration",
            config: redshift(json!({})),
            description_member: "RedshiftDestinationDescription",
            expected: vec![
                ("RetryOptions/DurationInSeconds", json!(3600)),
                ("S3BackupMode", json!("Disabled")),
                // The Amazon S3 location the destination delivers through carries the
                // buffering defaults of the plain S3 shape.
                ("S3DestinationDescription/BufferingHints/SizeInMBs", json!(5)),
                (
                    "S3DestinationDescription/BufferingHints/IntervalInSeconds",
                    json!(300),
                ),
            ],
        },
        // A supplied retry window of zero is a value, not an absence: it must not be read
        // as "nothing supplied" and replaced by the default.
        Case {
            case: "an Amazon Redshift destination that asked for no retries at all",
            stream: "redshift-no-retries",
            config_member: "RedshiftDestinationConfiguration",
            config: redshift(json!({ "RetryOptions": { "DurationInSeconds": 0 } })),
            description_member: "RedshiftDestinationDescription",
            expected: vec![("RetryOptions/DurationInSeconds", json!(0))],
        },
    ]
}

#[tokio::test]
async fn every_destination_description_reports_its_effective_buffering_retry_and_backup() {
    let emulator = Emulator::new();

    for case in cases() {
        let mut request = json!({ "DeliveryStreamName": case.stream });
        request[case.config_member] = case.config.clone();
        let (status, created) = emulator.call("CreateDeliveryStream", request).await;
        assert_eq!(
            status, 200,
            "case {:?}: create with {} got {created:?}",
            case.case, case.config_member
        );

        let (status, described) = emulator
            .call(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": case.stream }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {:?}: describe got {described:?}",
            case.case
        );
        // A Firehose stream has one destination, so the description under test is the
        // first.
        let description = described["DeliveryStreamDescription"]["Destinations"][0]
            [case.description_member]
            .clone();
        for (path, expected) in &case.expected {
            let actual = at(&description, path);
            assert_eq!(
                actual, expected,
                "case {:?}: expected {path} to be {expected:?}, got {actual:?} in \
                 {description:?}",
                case.case
            );
        }

        // Describing is reporting: reading again answers exactly the same effective
        // configuration, so nothing was written into the store by the first read.
        let (status, again) = emulator
            .call(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": case.stream }),
            )
            .await;
        assert_eq!(status, 200, "case {:?}: second describe got {again:?}", case.case);
        let repeated = again["DeliveryStreamDescription"]["Destinations"][0]
            [case.description_member]
            .clone();
        assert_eq!(
            repeated, description,
            "case {:?}: two reads in a row report the same destination, got {repeated:?}",
            case.case
        );
    }
}
