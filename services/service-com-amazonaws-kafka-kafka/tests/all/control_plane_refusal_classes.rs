// SPDX-License-Identifier: CC0-1.0

//! The CLASSES Amazon MSK's control-plane refusals fall into, and the sentence each one
//! carries, driven end to end through the shared `kafka_harness`.
//!
//! Three rules, each asserted over a TABLE rather than over one recorded request:
//!
//! 1. `CreateClusterV2` names EXACTLY ONE cluster type. A body naming both selects two
//!    clusters at once and is refused on the BODY with HTTP 400 `BadRequestException`, the
//!    sentence stating that only one of the two may be given and no `invalidParameter`,
//!    because either member would have fixed the request. The decision is taken on the
//!    SELECTION alone, ahead of anything inside either block, and no refused create registers
//!    a cluster.
//! 2. An operation that READS one cluster resolves the cluster reference it is handed, so
//!    every ARN of this service is such a reference: one naming another resource type names
//!    no cluster of the endpoint's region and is answered HTTP 404 `NotFoundException` with
//!    the service's fixed not-found sentence, while a value that is no MSK resource ARN at all
//!    stays the parameter refusal. Which of the two a surface answers is the operation's own:
//!    the cluster DELETE answers the parameter refusal for the same ARN, and the resource that
//!    ARN really names is served by its own reader throughout.
//! 3. The tagging operations refuse a `ResourceArn` they cannot act on in one fixed sentence
//!    that says WHY: a value that is not an ARN at all failed on its syntax, where a
//!    well-formed ARN of a kind the surface does not serve is refused as the reference it is.
//!    Neither message carries any part of the submitted value, and all three operations answer
//!    identically.
//!
//! The rows differ along what each rule generalises over: which blocks a create names and what
//! is in them (broker software, broker type, broker count, VPC configurations, tags, the
//! cluster name), which resource type an MSK ARN names and which reader is handed it, and how
//! a `ResourceArn` fails to be an ARN.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, ClusterRequest, Emulator, Reply, ACCOUNT, REGION};

/// MSK's sentence for a create whose body names both cluster types. Written out here rather
/// than read from the service's own constant, so this suite pins the wording instead of the
/// code under test.
const ONE_CLUSTER_TYPE_MESSAGE: &str =
    "You can specify only one cluster type, either provisioned or serverless.";

/// The service's one sentence for a resource it does not hold (the apostrophe is U+2019, as
/// MSK writes it).
const RESOURCE_NOT_FOUND_MESSAGE: &str = "The requested resource doesn\u{2019}t exist.";

/// The service's generic sentence for a parameter it will not accept.
const PARAMETERS_NOT_VALID_MESSAGE: &str = "One or more of the parameters are not valid.";

/// The sentence the tagging operations answer a `ResourceArn` that is not an ARN at all with.
const INVALID_TAG_ARN_SYNTAX_MESSAGE: &str = "Invalid input resource arn: bad syntax";

/// And the one they answer a well-formed ARN of a kind they do not serve with.
const INVALID_TAG_ARN_MESSAGE: &str = "Invalid input resource arn";

/// Broker software versions this service offers a NEW cluster, so a provisioned block naming
/// one is a block it would build. SOURCE: the valid-value list the service's own refusal
/// renders, re-exported as
/// `service_com_amazonaws_kafka_kafka::state::new_cluster_kafka_versions("kafka.m5.large")`.
const OFFERED_VERSIONS: [&str; 2] = ["3.8.x", "3.9.x"];

/// A `provisioned` block the service would build: `version` on `instance_type`, `brokers`
/// brokers over two distinct subnets.
fn provisioned(version: &str, instance_type: &str, brokers: u64) -> Value {
    json!({
        "kafkaVersion": version,
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": instance_type,
            "clientSubnets": ["subnet-aaaaaaaaaaaaaaaaa", "subnet-bbbbbbbbbbbbbbbbb"],
            "securityGroups": ["sg-aaaaaaaaaaaaaaaaa"],
            "storageInfo": { "ebsStorageInfo": { "volumeSize": 10 } },
        },
    })
}

/// A `serverless` block the service would build: `configurations` VPC configurations of one
/// subnet each, with the SASL/IAM client authentication MSK Serverless admits clients through.
fn serverless(configurations: usize) -> Value {
    json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({
                "subnetIds": [format!("subnet-{index:017x}")],
                "securityGroupIds": ["sg-bbbbbbbbbbbbbbbbb"],
            }))
            .collect::<Vec<Value>>(),
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// Assert `reply` is the body-level refusal: HTTP 400 `BadRequestException`, MSK's
/// selection sentence as the WHOLE message, and no member named.
fn assert_one_cluster_type(reply: &Reply, case: &str) {
    assert_eq!(
        (reply.status, reply.error_code(), reply.error_message()),
        (400, "BadRequestException", ONE_CLUSTER_TYPE_MESSAGE),
        "case {case}: expected a 400 BadRequestException {ONE_CLUSTER_TYPE_MESSAGE:?}, got {} {}",
        reply.status,
        reply.body
    );
    assert!(
        reply.at("/invalidParameter").is_null(),
        "case {case}: expected the body refusal to name no member, got {}",
        reply.body
    );
}

/// How many clusters `ListClustersV2` reports under `name`.
async fn listed_under(emulator: &Emulator, name: &str) -> usize {
    emulator
        .list_v2(&format!("?clusterNameFilter={}", pct(name)))
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

/// RULE 1: a `CreateClusterV2` body naming BOTH cluster types is refused with the sentence
/// stating that only one of the two may be given — whatever the two blocks ask for, and
/// whether or not a named block would itself have been refused, because the selection is
/// decided ahead of either block's own members. No such create registers a cluster, and a
/// body naming exactly one kind is served.
#[tokio::test]
async fn naming_both_cluster_types_states_the_rule_that_only_one_may_be_given() {
    let emulator = Emulator::new();

    // The rows differ in the broker software, the broker type, the broker count, how many VPC
    // configurations the serverless block names, what else the body carries, how long the
    // cluster name is, and whether a named block is one the service would itself have refused
    // (broker software it does not offer, a broker type it does not offer, a serverless block
    // naming no VPC configuration, one turning no client authentication on).
    let rows: Vec<(&str, Value)> = vec![
        (
            "a minimal block of each kind",
            json!({
                "clusterName": "both-minimal",
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.m5.large", 1),
                "serverless": serverless(1),
            }),
        ),
        (
            "two brokers over two subnets, one VPC configuration",
            json!({
                "clusterName": "both-two-brokers",
                "provisioned": provisioned(OFFERED_VERSIONS[1], "kafka.m5.large", 2),
                "serverless": serverless(1),
            }),
        ),
        (
            "another broker type, more brokers and more VPC configurations, with tags",
            json!({
                "clusterName": "both-larger",
                "tags": { "team": "data", "env": "sim2real" },
                "provisioned": provisioned(OFFERED_VERSIONS[1], "kafka.m7g.large", 6),
                "serverless": serverless(3),
            }),
        ),
        (
            "the longest cluster name the model admits",
            json!({
                "clusterName": "n".repeat(64),
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.m5.large", 2),
                "serverless": serverless(2),
            }),
        ),
        (
            "broker software the service does not offer",
            json!({
                "clusterName": "both-unoffered-version",
                "provisioned": provisioned("0.0.0-not-a-version", "kafka.m5.large", 2),
                "serverless": serverless(1),
            }),
        ),
        (
            "a broker type the service does not offer",
            json!({
                "clusterName": "both-unoffered-type",
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.not-a-type", 2),
                "serverless": serverless(1),
            }),
        ),
        (
            "a serverless block naming no VPC configuration",
            json!({
                "clusterName": "both-no-vpc-config",
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.m5.large", 2),
                "serverless": { "vpcConfigs": [], "clientAuthentication": { "sasl": { "iam": { "enabled": true } } } },
            }),
        ),
        (
            "a serverless block turning no client authentication on",
            json!({
                "clusterName": "both-unauthenticated",
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.m5.large", 2),
                "serverless": { "vpcConfigs": [{ "subnetIds": ["subnet-aaaaaaaaaaaaaaaaa"] }] },
            }),
        ),
    ];

    for (case, body) in &rows {
        let reply = emulator.create_cluster_v2(body.clone()).await;
        assert_one_cluster_type(&reply, case);
        let name = body["clusterName"].as_str().unwrap_or_default();
        assert_eq!(
            listed_under(&emulator, name).await,
            0,
            "case {case}: a refused create must register no cluster, got {} under {name:?}",
            listed_under(&emulator, name).await
        );
    }

    // The positive control: the SAME blocks are each served when the body names only one of
    // them, so the rows above refuse the selection and not the blocks.
    for (case, name, body) in [
        (
            "a provisioned create",
            "one-kind-provisioned",
            json!({
                "clusterName": "one-kind-provisioned",
                "provisioned": provisioned(OFFERED_VERSIONS[0], "kafka.m5.large", 2),
            }),
        ),
        (
            "a serverless create",
            "one-kind-serverless",
            json!({ "clusterName": "one-kind-serverless", "serverless": serverless(1) }),
        ),
    ] {
        let reply = emulator.create_cluster_v2(body).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: a create naming one cluster type must be served, got {} {}",
            reply.status, reply.body
        );
        assert_eq!(
            listed_under(&emulator, name).await,
            1,
            "case {case}: the served create must register its cluster"
        );
    }
}

/// The ARNs of another MSK resource type a cluster reader is handed below, beside the live
/// configuration ARN the first row carries: every resource type this service mints an ARN for
/// that is not a cluster's.
fn msk_arns_of_another_resource_type() -> Vec<(&'static str, String)> {
    let id = "11111111-2222-4333-8444-555555555555-1";
    vec![
        (
            "a configuration never minted",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/never-minted/{id}"),
        ),
        (
            "a replicator",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/a-replicator/{id}"),
        ),
        (
            "a VPC connection",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:vpc-connection/{ACCOUNT}/a-cluster/{id}"),
        ),
        (
            "a cluster operation",
            format!(
                "arn:aws:kafka:{REGION}:{ACCOUNT}:cluster-operation/a-cluster/{id}/\
                 99999999-8888-4777-8666-555555555555"
            ),
        ),
    ]
}

/// RULE 2: handed an ARN of THIS service that names another resource type, each operation
/// that reads one cluster answers it as a cluster that does not exist — HTTP 404
/// `NotFoundException` with the service's fixed sentence, carrying nothing of the request —
/// while a value that is no MSK resource ARN at all keeps the parameter refusal, the cluster
/// DELETE keeps it for the same ARN, and the resource the ARN really names is still served by
/// its own reader.
#[tokio::test]
async fn a_cluster_reader_answers_an_msk_arn_of_another_type_as_a_cluster_that_is_not_there() {
    let emulator = Emulator::new();
    // A configuration this endpoint really holds, so one row's ARN names a live resource of
    // another type rather than nothing at all.
    let configuration = emulator
        .create_configuration_ok(
            "refusal-classes-configuration",
            None,
            b"auto.create.topics.enable=true",
        )
        .await;
    // A cluster this endpoint holds, so the readers below are shown to serve what they can
    // resolve.
    let cluster = emulator
        .create_ok(&ClusterRequest::named("refusal-classes-cluster"))
        .await;

    let mut rows = vec![("a configuration this endpoint holds", configuration.clone())];
    rows.extend(msk_arns_of_another_resource_type());

    // Every operation that reads one cluster, with the URI the model binds it to.
    let readers: [(&str, &str, &str); 3] = [
        ("DescribeCluster", "/v1/clusters/", ""),
        ("DescribeClusterV2", "/api/v2/clusters/", ""),
        ("GetBootstrapBrokers", "/v1/clusters/", "/bootstrap-brokers"),
    ];

    for (value_case, arn) in &rows {
        for (reader, prefix, suffix) in readers {
            let case = format!("{reader} on {value_case}");
            let reply = emulator
                .call("GET", &format!("{prefix}{}{suffix}", pct(arn)), None)
                .await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (404, "NotFoundException", RESOURCE_NOT_FOUND_MESSAGE),
                "case {case}: expected the not-found refusal, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.at("/invalidParameter").as_str(),
                Some("clusterArn"),
                "case {case}: expected the refusal to name clusterArn, got {}",
                reply.body
            );
            for leaked in [arn.as_str(), REGION, ACCOUNT] {
                assert!(
                    !reply.error_message().contains(leaked),
                    "case {case}: the fixed sentence must not carry {leaked:?}, got {:?}",
                    reply.error_message()
                );
            }
        }

        // The cluster DELETE answers the SAME ARN as the parameter it cannot read: which
        // client error a surface answers for this class is the operation's own.
        let case = format!("DeleteCluster on {value_case}");
        let deleted = emulator
            .call("DELETE", &format!("/v1/clusters/{}", pct(arn)), None)
            .await;
        assert_eq!(
            (
                deleted.status,
                deleted.error_code(),
                deleted.error_message()
            ),
            (400, "BadRequestException", PARAMETERS_NOT_VALID_MESSAGE),
            "case {case}: expected the parameter refusal, got {} {}",
            deleted.status,
            deleted.body
        );
    }

    // A value that is no MSK resource ARN at all is still the parameter refusal on the
    // readers. Another VENDOR's resource is the row that shows it: a value that stands for no
    // resource at all is refused by authorization before any reader sees it, which is
    // `cluster_arn_refusal`'s rule rather than this one.
    for (case, arn) in [(
        "another vendor's resource",
        format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d"),
    )] {
        let reply = emulator
            .call(
                "GET",
                &format!("/api/v2/clusters/{}", pct(&arn)),
                None,
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (400, "BadRequestException", PARAMETERS_NOT_VALID_MESSAGE),
            "case DescribeClusterV2 on {case}: expected the parameter refusal, got {} {}",
            reply.status,
            reply.body
        );
    }

    // The controls: the configuration those refusals named is still served by its own reader,
    // and so is the cluster this endpoint holds.
    let described = emulator
        .call("GET", &format!("/v1/configurations/{}", pct(&configuration)), None)
        .await;
    assert_eq!(
        (
            described.status,
            described.at("/arn").as_str(),
            described.at("/name").as_str()
        ),
        (
            200,
            Some(configuration.as_str()),
            Some("refusal-classes-configuration")
        ),
        "case control: the configuration must still be served, got {} {}",
        described.status,
        described.body
    );
    let read = emulator.describe_v2(&cluster).await;
    assert_eq!(
        (read.status, read.at("/clusterInfo/clusterArn").as_str()),
        (200, Some(cluster.as_str())),
        "case control: the cluster this endpoint holds must still be served, got {} {}",
        read.status,
        read.body
    );
}

/// RULE 3: every tagging operation refuses a `ResourceArn` that is not an ARN at all on its
/// SYNTAX, in one sentence that carries no part of the submitted value — while a well-formed
/// ARN of a kind the surface does not serve reads the same sentence without that reason, so
/// the two input classes stay apart. A resource this service does tag round-trips through the
/// same three operations.
#[tokio::test]
async fn the_tagging_operations_refuse_a_value_that_is_no_arn_on_its_syntax() {
    let emulator = Emulator::new();

    // The rows differ along HOW the value fails to be an ARN: free text, a
    // truncated ARN, a missing partition, a missing service namespace, a long opaque value
    // and a non-ASCII one. Each row's value is longer than the words of the sentence, so a
    // sentence composed from the request would show.
    let not_an_arn: Vec<(&str, String)> = vec![
        ("free text", "not-an-arn".to_string()),
        ("free text with spaces", "not an arn at all".to_string()),
        ("a truncated ARN", "arn:aws:kafka".to_string()),
        (
            "no partition",
            format!("arn::kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/1"),
        ),
        (
            "no service namespace",
            format!("arn:aws::{REGION}:{ACCOUNT}:cluster/a-cluster/1"),
        ),
        (
            "an opaque value far longer than an ARN",
            format!("not-an-arn-{}", "x".repeat(200)),
        ),
        ("a non-ASCII value", "ニセのリソース-arn".to_string()),
    ];

    // And the neighbouring class: well-formed ARNs this surface cannot act on — another
    // vendor's, an MSK kind no API of this service gives a `Tags` member, and an MSK ARN
    // whose resource field names no resource.
    let not_a_taggable_resource: Vec<(&str, String)> = vec![
        (
            "another vendor's ARN",
            format!("arn:aws:sqs:{REGION}:{ACCOUNT}:a-queue"),
        ),
        (
            "an MSK configuration",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/a-configuration/1"),
        ),
        (
            "an MSK ARN naming no resource id",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster"),
        ),
    ];

    assert_ne!(
        INVALID_TAG_ARN_SYNTAX_MESSAGE, INVALID_TAG_ARN_MESSAGE,
        "the two input classes of the tagging surface must read different sentences"
    );
    for (expected, rows) in [
        (INVALID_TAG_ARN_SYNTAX_MESSAGE, &not_an_arn),
        (INVALID_TAG_ARN_MESSAGE, &not_a_taggable_resource),
    ] {
        for (row_case, arn) in rows {
            for (operation, method, uri, body) in [
                (
                    "TagResource",
                    "POST",
                    format!("/v1/tags/{}", pct(arn)),
                    Some(json!({ "tags": { "env": "sim2real" } })),
                ),
                (
                    "UntagResource",
                    "DELETE",
                    format!("/v1/tags/{}?tagKeys=env", pct(arn)),
                    None,
                ),
                (
                    "ListTagsForResource",
                    "GET",
                    format!("/v1/tags/{}", pct(arn)),
                    None,
                ),
            ] {
                let case = format!("{operation} on {row_case}");
                let reply = emulator.call(method, &uri, body).await;
                assert_eq!(
                    (reply.status, reply.error_code(), reply.error_message()),
                    (400, "BadRequestException", expected),
                    "case {case}: expected the fixed input refusal, got {} {}",
                    reply.status,
                    reply.body
                );
                assert!(
                    !reply.error_message().contains(arn.as_str()),
                    "case {case}: the sentence is fixed, so the submitted value must not reach \
                     it, got {:?}",
                    reply.error_message()
                );
            }
        }
    }

    // The positive control: a resource this service DOES tag round-trips through the same
    // three operations, so the refusals above are the reference being refused and not the
    // surface being closed.
    let cluster = emulator
        .create_ok(&ClusterRequest::named("tagging-control"))
        .await;
    let tagged = emulator
        .call(
            "POST",
            &format!("/v1/tags/{}", pct(&cluster)),
            Some(json!({ "tags": { "env": "sim2real" } })),
        )
        .await;
    assert_eq!(
        tagged.status, 204,
        "case control: expected TagResource to be served, got {} {}",
        tagged.status, tagged.body
    );
    let listed = emulator
        .call("GET", &format!("/v1/tags/{}", pct(&cluster)), None)
        .await;
    assert_eq!(
        (listed.status, listed.at("/tags/env").as_str()),
        (200, Some("sim2real")),
        "case control: expected the stored tag back, got {} {}",
        listed.status,
        listed.body
    );
    let removed = emulator
        .call(
            "DELETE",
            &format!("/v1/tags/{}?tagKeys=env", pct(&cluster)),
            None,
        )
        .await;
    assert_eq!(
        removed.status, 204,
        "case control: expected UntagResource to be served, got {} {}",
        removed.status, removed.body
    );
    let listed = emulator
        .call("GET", &format!("/v1/tags/{}", pct(&cluster)), None)
        .await;
    assert!(
        listed.at("/tags/env").is_null(),
        "case control: the removed tag must be gone, got {}",
        listed.body
    );
}
