// SPDX-License-Identifier: CC0-1.0

//! RE-configuring a cluster's audit-log delivery, end to end over the awsQuery
//! wire.
//!
//! The RULE every case here asserts: an accepted `EnableLogging` turns audit
//! logging on for the cluster it NAMES and leaves, as that cluster's whole
//! logging record, the delivery the request describes — the destination type,
//! the bucket, the key prefix and the exported log types — so a later request
//! REPLACES that record rather than merging into it, a member the new request
//! omits is reported by nobody afterwards, and no other cluster's record moves
//! ("Starts logging information, such as queries and connection attempts, for
//! the specified Amazon Redshift cluster", `smithy_query(query="doc",
//! shape="EnableLogging")`, reported back through `LoggingStatus`).
//!
//! A request that names no `LogDestinationType` asks for the destination
//! `BucketName` and `S3KeyPrefix` describe, which is `s3` (`smithy_query(
//! query="doc", shape="EnableLoggingMessage", member="LogDestinationType")` —
//! the enum's values are `s3`, `cloudwatch` and `s3table`).
//!
//! The rows differ along what the rule generalises over: which destination the
//! delivery is moved to, whether the request names a bucket, whether it names a
//! key prefix, how many log types it exports, and whether the bucket it moves to
//! is another one. The refusal table at the end pins the other half of the same
//! rule: a request that is REFUSED configures nothing, so the record the cluster
//! already carried is still the record it reports.
//!
//! Amazon S3 is mounted beside Redshift on ONE engine, because the bucket an S3
//! delivery names is a resource of THAT service: the buckets here are created
//! and policed through its own API, and every logging record is read back
//! through Redshift's own `DescribeLoggingStatus` rather than out of either
//! store.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_audit_logging::{
    create_bucket, delivery_policy, serving_cluster, DOCUMENTED_ACTIONS,
};
use crate::cluster_lifecycle::{element_blocks, Answer, Emulator};

/// The cluster every step below re-configures.
const CLUSTER: &str = "logging-reconfigured-cluster";

/// A second cluster, configured ONCE before anything re-configures the first and
/// re-read after every step: a logging record belongs to one cluster.
const OTHER_CLUSTER: &str = "logging-untouched-cluster";

/// The bucket the first steps deliver to, whose policy grants the log-delivery
/// principal both documented permissions.
const FIRST_BUCKET: &str = "redshift-reconfigured-logs-one";
/// A second bucket the account has, whose policy grants put-object alone — still
/// a policy that admits the delivery — so a step can move the delivery to
/// another bucket.
const SECOND_BUCKET: &str = "redshift-reconfigured-logs-two";
/// A bucket the account has that carries no policy at all, so a delivery to it is
/// refused.
const UNPOLICED_BUCKET: &str = "redshift-reconfigured-logs-unpoliced";
/// A well-formed bucket name the account has no bucket under.
const ABSENT_BUCKET: &str = "redshift-reconfigured-logs-absent";

/// The key prefix the second cluster's delivery is configured with, which no step
/// on the first cluster may disturb.
const OTHER_PREFIX: &str = "untouched/redshift/";

/// The whole of what one cluster's logging record reports once logging is on: the
/// members the request named, and the log types it exports in the order it listed
/// them.
#[derive(Debug)]
struct Record {
    bucket_name: Option<&'static str>,
    s3_key_prefix: Option<&'static str>,
    log_destination_type: &'static str,
    log_exports: &'static [&'static str],
}

/// One re-configuration: the `EnableLogging` members it sends beside the cluster
/// identifier, and the whole record it must leave behind.
struct Step {
    case: &'static str,
    params: &'static [(&'static str, &'static str)],
    record: Record,
}

/// The re-configurations, applied to ONE cluster in this order. Each row is read
/// against the record the row before it left, so every row that omits a member
/// the previous row named is a case of a replaced record rather than a merged
/// one.
///
/// The log types are the ones the operation publishes for an `s3` or a
/// `cloudwatch` destination — `connectionlog`, `useractivitylog` and `userlog`;
/// an `s3table` destination exports "the names of the system tables to publish"
/// instead (`smithy_query(query="doc", shape="EnableLoggingMessage",
/// member="LogExports")`).
const STEPS: &[Step] = &[
    Step {
        case: "an S3 delivery of two log types under a key prefix",
        params: &[
            ("BucketName", FIRST_BUCKET),
            ("S3KeyPrefix", "audit/redshift/"),
            ("LogExports.member.1", "connectionlog"),
            ("LogExports.member.2", "userlog"),
        ],
        record: Record {
            bucket_name: Some(FIRST_BUCKET),
            s3_key_prefix: Some("audit/redshift/"),
            // The request named no destination type, so it asked for the one
            // `BucketName` and `S3KeyPrefix` describe.
            log_destination_type: state::LOG_DESTINATION_S3,
            log_exports: &["connectionlog", "userlog"],
        },
    },
    Step {
        case: "the same bucket with neither a prefix nor a log type",
        params: &[("BucketName", FIRST_BUCKET)],
        record: Record {
            bucket_name: Some(FIRST_BUCKET),
            s3_key_prefix: None,
            log_destination_type: state::LOG_DESTINATION_S3,
            log_exports: &[],
        },
    },
    Step {
        case: "a CloudWatch delivery of one log type",
        params: &[
            ("LogDestinationType", state::LOG_DESTINATION_CLOUDWATCH),
            ("LogExports.member.1", "useractivitylog"),
        ],
        record: Record {
            bucket_name: None,
            s3_key_prefix: None,
            log_destination_type: state::LOG_DESTINATION_CLOUDWATCH,
            log_exports: &["useractivitylog"],
        },
    },
    Step {
        case: "an S3 Tables delivery of three system tables",
        params: &[
            ("LogDestinationType", state::LOG_DESTINATION_S3_TABLE),
            ("LogExports.member.1", "stl_query"),
            ("LogExports.member.2", "stl_connectionlog"),
            ("LogExports.member.3", "svl_statementtext"),
        ],
        record: Record {
            bucket_name: None,
            s3_key_prefix: None,
            log_destination_type: state::LOG_DESTINATION_S3_TABLE,
            log_exports: &["stl_query", "stl_connectionlog", "svl_statementtext"],
        },
    },
    Step {
        case: "an S3 delivery moved to another bucket, under a prefix of the documented characters",
        params: &[
            ("LogDestinationType", state::LOG_DESTINATION_S3),
            ("BucketName", SECOND_BUCKET),
            ("S3KeyPrefix", "audit=logs/cluster-1/"),
            ("LogExports.member.1", "userlog"),
        ],
        record: Record {
            bucket_name: Some(SECOND_BUCKET),
            s3_key_prefix: Some("audit=logs/cluster-1/"),
            log_destination_type: state::LOG_DESTINATION_S3,
            log_exports: &["userlog"],
        },
    },
    Step {
        case: "a CloudWatch delivery exporting nothing",
        params: &[("LogDestinationType", state::LOG_DESTINATION_CLOUDWATCH)],
        record: Record {
            bucket_name: None,
            s3_key_prefix: None,
            log_destination_type: state::LOG_DESTINATION_CLOUDWATCH,
            log_exports: &[],
        },
    },
];

/// The record the second cluster is configured with and must keep throughout.
const OTHER_RECORD: Record = Record {
    bucket_name: Some(FIRST_BUCKET),
    s3_key_prefix: Some(OTHER_PREFIX),
    log_destination_type: state::LOG_DESTINATION_S3,
    log_exports: &["userlog"],
};

/// An emulator with Amazon S3 mounted, the two clusters these cases configure
/// available, and the buckets the rows name created and policed.
async fn harness() -> Emulator {
    let emulator = Emulator::with_s3();
    serving_cluster(&emulator, "the re-configured cluster", CLUSTER).await;
    serving_cluster(&emulator, "the cluster beside it", OTHER_CLUSTER).await;
    create_bucket(
        &emulator,
        FIRST_BUCKET,
        Some(delivery_policy(FIRST_BUCKET, &DOCUMENTED_ACTIONS, true)),
    )
    .await;
    create_bucket(
        &emulator,
        SECOND_BUCKET,
        Some(delivery_policy(SECOND_BUCKET, &["s3:PutObject"], false)),
    )
    .await;
    create_bucket(&emulator, UNPOLICED_BUCKET, None).await;
    emulator
}

/// `EnableLogging` for `cluster` with these members beside its identifier.
async fn enable_logging(
    emulator: &Emulator,
    cluster: &str,
    params: &[(&str, &str)],
) -> Answer {
    let mut sent: Vec<(&str, &str)> = vec![("ClusterIdentifier", cluster)];
    sent.extend_from_slice(params);
    emulator.call("EnableLogging", &sent).await
}

/// `DescribeLoggingStatus` for `cluster`.
async fn logging_status(emulator: &Emulator, cluster: &str) -> Answer {
    emulator
        .call("DescribeLoggingStatus", &[("ClusterIdentifier", cluster)])
        .await
}

/// The log types one answer reports, in document order.
///
/// Read through [`element_blocks`] rather than through an exact `<member>` match
/// because the framework's serializer writes the service's XML namespace onto the
/// first entry of a list, so that entry's open tag carries an attribute.
fn reported_log_exports(body: &str) -> Vec<String> {
    element_blocks(body, "member")
}

/// Assert that `answer` reports logging ON with exactly `expected`.
fn assert_reports(case: &str, source: &str, answer: &Answer, expected: &Record) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from {source}, got {} with body {}",
        answer.status, answer.body
    );
    for (member, value) in [
        ("LoggingEnabled", Some("true")),
        ("BucketName", expected.bucket_name),
        ("S3KeyPrefix", expected.s3_key_prefix),
        ("LogDestinationType", Some(expected.log_destination_type)),
        // Nothing has delivered a log file, so there is no delivery or failure
        // for a time or a message to report.
        ("LastSuccessfulDeliveryTime", None),
        ("LastFailureTime", None),
        ("LastFailureMessage", None),
    ] {
        assert_eq!(
            answer.text(member).as_deref(),
            value,
            "case {case:?}: expected {member}={value:?} on {source}, got {:?} in body {}",
            answer.text(member),
            answer.body
        );
    }
    let exports: Vec<String> = expected
        .log_exports
        .iter()
        .map(|export| export.to_string())
        .collect();
    assert_eq!(
        reported_log_exports(&answer.body),
        exports,
        "case {case:?}: expected the exported log types {exports:?} on {source}, got body {}",
        answer.body
    );
}

#[tokio::test]
async fn each_accepted_request_replaces_the_whole_record_of_the_cluster_it_names() {
    let emulator = harness().await;

    let seeded = enable_logging(
        &emulator,
        OTHER_CLUSTER,
        &[
            ("BucketName", FIRST_BUCKET),
            ("S3KeyPrefix", OTHER_PREFIX),
            ("LogExports.member.1", "userlog"),
        ],
    )
    .await;
    assert_reports(
        "the cluster beside it",
        "its own EnableLogging",
        &seeded,
        &OTHER_RECORD,
    );

    for step in STEPS {
        let answered = enable_logging(&emulator, CLUSTER, step.params).await;
        assert_reports(step.case, "the EnableLogging answer", &answered, &step.record);

        // The same record, read back through the reader that reports it: what
        // the call answered with is what the cluster now carries.
        let read_back = logging_status(&emulator, CLUSTER).await;
        assert_reports(step.case, "the read-back status", &read_back, &step.record);

        // And the cluster beside it still carries the delivery IT was configured
        // with: a configuration is a configuration of one cluster.
        let beside = logging_status(&emulator, OTHER_CLUSTER).await;
        assert_reports(
            step.case,
            "the read-back status of the cluster beside it",
            &beside,
            &OTHER_RECORD,
        );
    }
}

/// One refusal of a re-configuration: what it names beside the cluster
/// identifier, and the status and error code it is refused with.
///
/// The codes are the modelled faults of the operation
/// (`smithy_query(query="operation", operation="EnableLogging")`).
struct Refusal {
    case: &'static str,
    params: &'static [(&'static str, &'static str)],
    status: u16,
    code: &'static str,
}

/// The rows the refusal table spans, differing along WHICH precondition of the
/// request fails: the bucket name itself, the account not holding the bucket, the
/// bucket's policy admitting nothing, the request naming no bucket for an S3
/// delivery at all, and the cluster not being one the account has.
const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a well-formed bucket name the account has no bucket under",
        params: &[("BucketName", ABSENT_BUCKET)],
        status: 400,
        code: "BucketNotFoundFault",
    },
    Refusal {
        // "must not contain two adjacent periods" — one sentence of Amazon S3's
        // own published bucket naming rules, so this name names no bucket
        // anywhere.
        case: "a bucket name carrying two adjacent periods",
        params: &[("BucketName", "reconfigured..logs")],
        status: 400,
        code: "InvalidS3BucketNameFault",
    },
    Refusal {
        case: "a bucket the account has that carries no policy at all",
        params: &[("BucketName", UNPOLICED_BUCKET)],
        status: 400,
        code: "InsufficientS3BucketPolicyFault",
    },
    Refusal {
        case: "an S3 delivery naming no bucket at all",
        params: &[("LogDestinationType", state::LOG_DESTINATION_S3)],
        status: 400,
        code: "InvalidS3BucketNameFault",
    },
];

#[tokio::test]
async fn a_refused_request_leaves_the_record_the_cluster_already_carried() {
    let emulator = harness().await;

    // The delivery every row below tries, and fails, to replace.
    let configured = &STEPS[0];
    let seeded = enable_logging(&emulator, CLUSTER, configured.params).await;
    assert_reports(
        configured.case,
        "the EnableLogging answer",
        &seeded,
        &configured.record,
    );

    for row in REFUSALS {
        let answered = enable_logging(&emulator, CLUSTER, row.params).await;
        assert_eq!(
            (answered.status, answered.error_code().as_deref()),
            (row.status, Some(row.code)),
            "case {:?}: expected {} {}, got {} / {:?} with body {}",
            row.case,
            row.status,
            row.code,
            answered.status,
            answered.error_code(),
            answered.body
        );
        assert_reports(
            row.case,
            "the read-back status after the refusal",
            &logging_status(&emulator, CLUSTER).await,
            &configured.record,
        );
    }

    // A request naming a cluster the account does not have configures no
    // cluster, and the one that IS configured is untouched by it.
    let absent = enable_logging(
        &emulator,
        "logging-reconfigured-absent",
        &[("BucketName", FIRST_BUCKET)],
    )
    .await;
    assert_eq!(
        (absent.status, absent.error_code().as_deref()),
        (404, Some("ClusterNotFound")),
        "case a cluster the account does not have: expected 404 ClusterNotFound, got {} / {:?} \
         with body {}",
        absent.status,
        absent.error_code(),
        absent.body
    );
    assert_reports(
        "a cluster the account does not have",
        "the read-back status of the configured cluster",
        &logging_status(&emulator, CLUSTER).await,
        &configured.record,
    );
}
