// SPDX-License-Identifier: CC0-1.0

//! The FIELD a refused group read reports its error in, at the version boundary the
//! protocol draws.
//!
//! RULE under test: an error the broker must report is carried in a field the NEGOTIATED
//! version of the response defines. Apache Kafka's `OffsetFetchResponse` gained its
//! top-level `ErrorCode` in version 2; every version from 1 on carries a per-partition
//! one. So a refused `OffsetFetch` states the refusal on each partition the request named
//! whatever version it negotiated, and at the top level as well from version 2 on — where
//! stating it only at the top level leaves a client on version 1 reading a group with
//! nothing committed, which is exactly what a group it IS allowed to read answers.
//!
//! The rows are the two versions that boundary separates, each driven for a group the
//! signing identity's policies cover and one they do not — so the suite spans both the
//! version and the decision.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at the
//! SASL/IAM address `GetBootstrapBrokers` published.

use ::kafka_protocol::messages::{
    offset_fetch_request::OffsetFetchRequestTopic, ApiKey, GroupId, OffsetFetchRequest,
    OffsetFetchResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;
use serde_json::json;

use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, ClusterRequest, Emulator, ACCOUNT, REGION,
};
use crate::non_java_client_group_contracts::{
    given_active_topic, iam_client, CONNECT, DESCRIBE_TOPIC, READ_DATA, WRITE_DATA,
};

/// The topic the read names, the group the policy covers and the one it does not.
const TOPIC: &str = "orders";
const COVERED_GROUP: &str = "offsets-covered";
const UNCOVERED_GROUP: &str = "offsets-uncovered";

/// The version of `OffsetFetchResponse` that first carries a top-level error code.
const TOP_LEVEL_ERROR_FROM: i16 = 2;

/// RULE: a refused `OffsetFetch` reports its error on every partition the request named at
/// every version, and at the top level from the version that has one — so no client reads a
/// refusal as an empty, allowed answer.
#[tokio::test]
async fn a_refused_offset_fetch_reports_its_error_at_every_negotiated_version() {
    let case = "a group the signing identity may not describe";
    let emulator = Emulator::with_iam();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_iam: Some(true),
            ..ClusterRequest::named("offset-fetch-versions")
        })
        .await;
    given_active_topic(&emulator, &cluster, TOPIC, 1, case).await;
    let access_key_id = "ASIAOFFSETFETCHVERSIONS";
    given_assumed_role_session(&emulator.engine, "offset-fetch-role", "session-1", access_key_id);
    given_role_inline_policy(
        &emulator.engine,
        "offset-fetch-role",
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": [CONNECT, WRITE_DATA, READ_DATA, DESCRIBE_TOPIC],
                    "Resource": [
                        format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*"),
                        format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*"),
                    ],
                },
                {
                    "Effect": "Allow",
                    "Action": ["kafka-cluster:DescribeGroup"],
                    "Resource": [
                        format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*/{COVERED_GROUP}"),
                    ],
                },
            ],
        })),
    );
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let mut client = iam_client(case, &authority, access_key_id).await;

    let refused = ResponseError::GroupAuthorizationFailed.code();
    // (the version negotiated, the group the read names, the error the refusal carries)
    let rows: &[(i16, &str, i16)] = &[
        (1, COVERED_GROUP, 0),
        (1, UNCOVERED_GROUP, refused),
        (TOP_LEVEL_ERROR_FROM, COVERED_GROUP, 0),
        (TOP_LEVEL_ERROR_FROM, UNCOVERED_GROUP, refused),
    ];
    for (version, group, expected) in rows {
        let probe = format!("{case} / OffsetFetch v{version} on {group}");
        let answered: OffsetFetchResponse = client
            .call(
                &probe,
                ApiKey::OffsetFetch,
                *version,
                &OffsetFetchRequest::default()
                    .with_group_id(GroupId(StrBytes::from_static_str(group)))
                    .with_topics(Some(vec![OffsetFetchRequestTopic::default()
                        .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                        .with_partition_indexes(vec![0])])),
            )
            .await;
        let partition = answered
            .topics
            .first()
            .and_then(|topic| topic.partitions.first())
            .unwrap_or_else(|| panic!("case {probe:?}: OffsetFetch reported no partition"))
            .clone();
        assert_eq!(
            partition.error_code, *expected,
            "case {probe:?}: expected the partition to report {expected}, got {}",
            partition.error_code
        );
        let top_level = match *version >= TOP_LEVEL_ERROR_FROM {
            true => *expected,
            false => 0,
        };
        assert_eq!(
            answered.error_code, top_level,
            "case {probe:?}: expected {top_level} at the top level, got {}",
            answered.error_code
        );
    }
}
