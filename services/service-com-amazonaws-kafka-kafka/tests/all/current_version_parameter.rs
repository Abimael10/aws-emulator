// SPDX-License-Identifier: CC0-1.0

//! The `CurrentVersion` PARAMETER rule of this service's cluster family, driven
//! end to end over the same `MultiServiceRouter` the emulator serves from,
//! through the shared harness in `tests/all/kafka_harness.rs`.
//!
//! RULE under test: a `CurrentVersion` is a token the SERVICE mints and the
//! caller quotes back, so a value outside that shape names no version of any
//! cluster — every operation that quotes one refuses such a request with HTTP 400
//! `BadRequestException` carrying the service's fixed invalid-parameters sentence
//! and naming `currentVersion` as the parameter at fault, and it answers that
//! refusal BEFORE any refusal about the cluster itself: its kind, its state, and
//! the version it is at. A token of that shape the cluster is simply not at is
//! the OTHER class, the stale-version refusal. A refused call changes nothing.
//!
//! The rows differ along what the rule generalises over: which operation quotes
//! the version — every one of the family, across three HTTP methods and a query
//! parameter — and HOW the value fails to be a token: no prefix, empty, wrong
//! case, a character the token alphabet does not carry, and the token length one
//! under, on, and one over. Whether a value IS a token is asserted through the
//! service's own predicate, so this suite and the handlers cannot drift apart on
//! the shape.

use serde_json::{json, Value};

// The service's own predicate, refusal wordings and parameter name, so the suite
// and the handlers cannot drift apart on what the service says.
use service_com_amazonaws_kafka_kafka::state::{
    is_current_version_token, BAD_REQUEST_MESSAGE, CLUSTER_VERSION_NOT_CURRENT_MESSAGE,
    CURRENT_VERSION_MEMBER, PROVISIONED_ONLY_MESSAGE,
};

use crate::kafka_harness::{
    assert_cluster_state_refusal, pct, poll_until_active, sleep_past_activation, ClusterRequest,
    ConfigurationRequest, Emulator, Reply, REGION,
};

/// A token of the shape the service mints that no cluster of a fresh registry is
/// at — the value every row that tests the OTHER class quotes. Its shape is
/// asserted through [`is_current_version_token`], never assumed.
const OTHER_TOKEN: &str = "K0000000000000";

/// Every way a quoted value can fail to be a token of that shape: `(case,
/// value)`. The shapes come from the service's own token form — a `K` followed by
/// upper-case alphanumerics, 14 characters in all — so the rows cover its prefix,
/// its alphabet, its case and its length, including the length one under, and one
/// over, the one the service mints.
fn not_tokens() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a bare digit", "1"),
        ("empty", ""),
        ("prose", "not-a-version"),
        ("the token in lower case", "k0000000000000"),
        ("a lower-case tail", "K000000000000a"),
        ("a character outside the alphabet", "K000000000000-"),
        ("another prefix", "X0000000000000"),
        ("one character short", "K000000000000"),
        ("one character long", "K00000000000000"),
    ]
}

/// One request of the cluster-version family.
struct Request {
    /// The operation, as the model names it — the label of every case below.
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// Every operation of this service that quotes a CLUSTER's `CurrentVersion`,
/// each quoting `version`, with routes and members as the model binds them
/// (`smithy_query(query="operation", operation="UpdateRebalancing,
/// UpdateMonitoring,UpdateStorage,UpdateBrokerCount,UpdateBrokerType,
/// UpdateBrokerStorage,UpdateSecurity,UpdateConnectivity,
/// UpdateClusterConfiguration,UpdateClusterKafkaVersion,DeleteCluster")`).
///
/// Every other member of each body is one the operation accepts, so the only
/// thing a row varies is the version: an operation that could change nothing, or
/// whose other values are out of range, is refused on THAT first and would say
/// nothing about this rule.
fn family(cluster_arn: &str, configuration_arn: &str, version: &str) -> Vec<Request> {
    let cluster = pct(cluster_arn);
    vec![
        Request {
            operation: "UpdateRebalancing",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/rebalancing"),
            body: Some(json!({
                "currentVersion": version,
                "rebalancing": { "status": "PAUSED" }
            })),
        },
        Request {
            operation: "UpdateMonitoring",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/monitoring"),
            body: Some(json!({
                "currentVersion": version,
                "enhancedMonitoring": "PER_BROKER"
            })),
        },
        Request {
            operation: "UpdateStorage",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/storage"),
            body: Some(json!({ "currentVersion": version, "volumeSizeGB": 2000 })),
        },
        Request {
            operation: "UpdateBrokerStorage",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/nodes/storage"),
            body: Some(json!({
                "currentVersion": version,
                "targetBrokerEBSVolumeInfo": [
                    { "kafkaBrokerNodeId": "ALL", "volumeSizeGB": 2000 }
                ]
            })),
        },
        Request {
            operation: "UpdateBrokerCount",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/nodes/count"),
            body: Some(json!({
                "currentVersion": version,
                "targetNumberOfBrokerNodes": 6
            })),
        },
        Request {
            operation: "UpdateBrokerType",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/nodes/type"),
            body: Some(json!({
                "currentVersion": version,
                "targetInstanceType": "kafka.m5.xlarge"
            })),
        },
        Request {
            operation: "UpdateSecurity",
            method: "PATCH",
            uri: format!("/v1/clusters/{cluster}/security"),
            body: Some(json!({
                "currentVersion": version,
                "encryptionInfo": {
                    "encryptionInTransit": { "clientBroker": "TLS" }
                }
            })),
        },
        Request {
            operation: "UpdateConnectivity",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/connectivity"),
            body: Some(json!({
                "currentVersion": version,
                "connectivityInfo": { "networkType": "DUAL" }
            })),
        },
        Request {
            operation: "UpdateClusterConfiguration",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/configuration"),
            body: Some(json!({
                "currentVersion": version,
                "configurationInfo": { "arn": configuration_arn, "revision": 1 }
            })),
        },
        Request {
            operation: "UpdateClusterKafkaVersion",
            method: "PUT",
            uri: format!("/v1/clusters/{cluster}/version"),
            body: Some(json!({
                "currentVersion": version,
                "targetKafkaVersion": "3.7.x"
            })),
        },
        Request {
            // The one operation that carries the version as a query parameter
            // rather than in a body.
            operation: "DeleteCluster",
            method: "DELETE",
            uri: format!(
                "/v1/clusters/{cluster}?{CURRENT_VERSION_MEMBER}={}",
                pct(version)
            ),
            body: None,
        },
    ]
}

/// Assert ONE refusal: HTTP 400 `BadRequestException`, `message` as the whole
/// sentence, `currentVersion` as the parameter at fault, and nothing of the
/// quoted value echoed back.
fn assert_refused(reply: &Reply, case: &str, message: &str, quoted: &str) {
    assert_eq!(
        (
            reply.status,
            reply.error_code(),
            reply.error_message(),
            reply.at("/invalidParameter").as_str(),
        ),
        (
            400,
            "BadRequestException",
            message,
            Some(CURRENT_VERSION_MEMBER),
        ),
        "case {case}: expected the refusal {message:?} naming {CURRENT_VERSION_MEMBER}, got {} {}",
        reply.status,
        reply.body
    );
    if !quoted.is_empty() {
        assert!(
            !reply.error_message().contains(quoted),
            "case {case}: the refusal must not echo the quoted version, got {:?}",
            reply.error_message()
        );
    }
}

/// RULE: a value that is not a token of the shape the service mints is refused as
/// a PARAMETER by every operation that quotes a cluster's version, in the
/// service's fixed invalid-parameters sentence, and changes nothing.
#[tokio::test]
async fn a_quoted_version_outside_the_token_shape_is_refused_by_every_operation() {
    let emulator = Emulator::new();
    let configuration = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("version-parameter"))
        .await;
    let arn = emulator
        .create_ok(&ClusterRequest::named("version-parameter"))
        .await;
    let version = poll_until_active(&emulator, &arn, "version parameter").await;

    for (shape, quoted) in not_tokens() {
        // The table is checked against the SERVICE's own predicate, so a row
        // cannot claim a shape the service would accept as a version.
        assert!(
            !is_current_version_token(quoted),
            "case {shape}: the row must not be a version token of this service, got {quoted:?}"
        );
        for request in family(&arn, &configuration, quoted) {
            let case = format!("{}/{shape}", request.operation);
            let reply = emulator
                .call(request.method, &request.uri, request.body)
                .await;
            assert_refused(&reply, &case, BAD_REQUEST_MESSAGE, quoted);
        }
    }

    // Nothing the refusals touched: the cluster is still there, still ACTIVE, at
    // the version it was at — no delete went through and no update started.
    let after = emulator.describe(&arn).await;
    assert_eq!(
        (
            after.status,
            after.at("/clusterInfo/state").as_str(),
            after.at("/clusterInfo/currentVersion").as_str(),
            after.at("/clusterInfo/activeOperationArn").as_str(),
        ),
        (200, Some("ACTIVE"), Some(version.as_str()), None),
        "a refused call must leave the cluster exactly as it was, got {} {}",
        after.status,
        after.body
    );
}

/// RULE: a token OF that shape which the cluster is not at is the other class —
/// the stale-version refusal — from every one of the same operations, so the
/// parameter class is about the value's shape and the stale class about the
/// cluster's version.
#[tokio::test]
async fn a_token_the_cluster_is_not_at_is_the_stale_version_refusal() {
    let emulator = Emulator::new();
    let configuration = emulator
        .create_configuration_row_ok_in(REGION, &ConfigurationRequest::named("stale-token"))
        .await;
    let arn = emulator.create_ok(&ClusterRequest::named("stale-token")).await;
    let version = poll_until_active(&emulator, &arn, "stale token").await;
    assert!(
        is_current_version_token(OTHER_TOKEN) && OTHER_TOKEN != version,
        "the stale quote must be a token of the service's shape that this cluster is not at, \
         stored {version:?}"
    );

    for request in family(&arn, &configuration, OTHER_TOKEN) {
        let case = format!("{}/a token this cluster is not at", request.operation);
        let reply = emulator
            .call(request.method, &request.uri, request.body)
            .await;
        assert_refused(
            &reply,
            &case,
            CLUSTER_VERSION_NOT_CURRENT_MESSAGE,
            OTHER_TOKEN,
        );
    }

    let after = emulator.describe(&arn).await;
    assert_eq!(
        (
            after.status,
            after.at("/clusterInfo/state").as_str(),
            after.at("/clusterInfo/currentVersion").as_str(),
        ),
        (200, Some("ACTIVE"), Some(version.as_str())),
        "a refused call must leave the cluster exactly as it was, got {} {}",
        after.status,
        after.body
    );
}

/// RULE: the parameter refusal pre-empts every refusal about the cluster itself —
/// the KIND of cluster the request addresses (a serverless cluster, which has no
/// brokers of its own), the STATE it is in (a cluster still being provisioned)
/// and the version it will accept its next change from (a cluster that has
/// already SPENT its version on a change it accepted) are each answered only for
/// a request whose version this service can read at all.
#[tokio::test]
async fn the_parameter_refusal_pre_empts_the_refusals_about_the_cluster() {
    let emulator = Emulator::new();

    // A serverless cluster is serverless for good, so this half of the rule
    // depends on no clock: with a version this service cannot read, the parameter
    // refusal answers; with a token it can, the refusal about the cluster's kind
    // does.
    let serverless = emulator
        .create_serverless_ok(
            "version-parameter-serverless",
            &[vec!["subnet-a", "subnet-b"]],
            Some(true),
        )
        .await;
    sleep_past_activation();
    let monitoring = format!("/v1/clusters/{}/monitoring", pct(&serverless));
    let unreadable = emulator
        .call(
            "PUT",
            &monitoring,
            Some(json!({ "currentVersion": "1", "enhancedMonitoring": "PER_BROKER" })),
        )
        .await;
    assert_refused(
        &unreadable,
        "serverless/a bare digit",
        BAD_REQUEST_MESSAGE,
        "1",
    );
    let readable = emulator
        .call(
            "PUT",
            &monitoring,
            Some(json!({ "currentVersion": OTHER_TOKEN, "enhancedMonitoring": "PER_BROKER" })),
        )
        .await;
    assert_eq!(
        (
            readable.status,
            readable.error_code(),
            readable.error_message()
        ),
        (400, "BadRequestException", PROVISIONED_ONLY_MESSAGE),
        "case serverless/a token: expected the refusal about the cluster's kind, got {} {}",
        readable.status,
        readable.body
    );

    // And over the refusals the cluster's own record decides. A cluster still
    // being provisioned is in no state a change is accepted from, and one whose
    // accepted change is still in flight has SPENT the version that change was
    // accepted from — accepting it generated the cluster's next version — so a
    // caller quoting the spent one again is told its version is not current. Either
    // way a version this service cannot read at all is answered on the request
    // instead.
    let arn = emulator
        .create_ok(&ClusterRequest::named("version-parameter-state"))
        .await;
    let creating_version = emulator
        .describe(&arn)
        .await
        .at("/clusterInfo/currentVersion")
        .as_str()
        .unwrap_or_else(|| panic!("case creating: the cluster reported no currentVersion"))
        .to_string();
    let rebalancing = format!("/v1/clusters/{}/rebalancing", pct(&arn));
    let rebalance = |version: &str| {
        json!({
            "currentVersion": version,
            "rebalancing": { "status": "PAUSED" }
        })
    };
    let unreadable = emulator
        .call("PUT", &rebalancing, Some(rebalance("1")))
        .await;
    assert_refused(&unreadable, "creating/a bare digit", BAD_REQUEST_MESSAGE, "1");
    let readable = emulator
        .call("PUT", &rebalancing, Some(rebalance(&creating_version)))
        .await;
    assert_cluster_state_refusal(&readable, "creating/the cluster's own version");

    let version = poll_until_active(&emulator, &arn, "version parameter state").await;
    let accepted = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/monitoring", pct(&arn)),
            Some(json!({ "currentVersion": version, "enhancedMonitoring": "PER_BROKER" })),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "expected the update quoting the cluster's own version to be accepted, got {} {}",
        accepted.status, accepted.body
    );
    let unreadable = emulator
        .call("PUT", &rebalancing, Some(rebalance("1")))
        .await;
    assert_refused(&unreadable, "updating/a bare digit", BAD_REQUEST_MESSAGE, "1");
    let readable = emulator
        .call("PUT", &rebalancing, Some(rebalance(&version)))
        .await;
    assert_refused(
        &readable,
        "updating/the version the accepted update spent",
        CLUSTER_VERSION_NOT_CURRENT_MESSAGE,
        &version,
    );
}
