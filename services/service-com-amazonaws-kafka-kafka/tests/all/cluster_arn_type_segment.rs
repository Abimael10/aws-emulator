// SPDX-License-Identifier: CC0-1.0

//! The LAST SEGMENT of a minted cluster ARN says which KIND of cluster the ARN names.
//!
//! RULE: Amazon MSK mints a cluster ARN as
//! `arn:aws:kafka:<region>:<account>:cluster/<name>/<uuid>-<type segment>`, and the segment
//! after the cluster's uuid encodes the cluster's TYPE — a SERVERLESS cluster carries the
//! serverless marker `s2`, a PROVISIONED one carries its own decimal ordinal — so a client
//! that reads the cluster type out of the ARN (an IAM resource pattern scoped to
//! `cluster/*/*-s2`, a console, a physical resource id) classifies the cluster as the kind
//! it really is. The model places no constraint on the ARN text, so this is recorded
//! behaviour rather than something `smithy_query` states; what the model does fix is that
//! the type itself is one of the two `ClusterType` values a reader reports
//! (`smithy_query(query="shape", shape="com.amazonaws.kafka#ClusterType")`).
//!
//! The rows differ along what the rule generalises over: the cluster KIND, which create
//! surface mints it (`CreateCluster`, `CreateClusterV2`'s two bodies), the ORDER the
//! creates are made in (so the registry's ordinal advances between kinds and no row's
//! segment can come from its position), the region the endpoint serves, the number of VPC
//! configurations and subnets a serverless body names, and the broker count and Kafka
//! version a provisioned body names.
//!
//! Every ARN here is asserted against the form above and against the `ClusterType` the
//! readers report for the SAME ARN, so the segment is checked against the registry's own
//! answer rather than against the request that was sent.

use serde_json::{json, Value};

use crate::kafka_harness::{pct, ClusterRequest, Emulator, ACCOUNT, REGION};

/// The segment a SERVERLESS cluster's ARN carries after its uuid.
const SERVERLESS_SEGMENT: &str = "s2";

/// Which kind of cluster a row asks for, and so which segment its ARN must end in and
/// which `ClusterType` the readers must report for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Provisioned,
    Serverless,
}

impl Kind {
    /// The `ClusterType` a reader reports for this kind.
    fn cluster_type(self) -> &'static str {
        match self {
            Self::Provisioned => "PROVISIONED",
            Self::Serverless => "SERVERLESS",
        }
    }
}

/// One row: the kind, the cluster name, the region whose endpoint the create is sent to,
/// and the surface it is sent over.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    kind: Kind,
    name: &'static str,
    region: &'static str,
    /// For a serverless row, one subnet list per `VpcConfig` the create names; for a
    /// provisioned row, the client subnets its broker node group names.
    subnets: Vec<Vec<&'static str>>,
    /// How many brokers a provisioned row asks for, and the Kafka version it asks for.
    brokers: i64,
    kafka_version: &'static str,
    /// Whether a provisioned row goes over `CreateClusterV2` rather than `CreateCluster`:
    /// both mint the same record, so both must mint the same ARN form.
    over_v2: bool,
}

/// The `Serverless` body of a row: one `VpcConfig` per subnet list, with the SASL/IAM
/// client authentication MSK admits a serverless cluster's clients through (a serverless
/// create that does not turn it on is refused, which is
/// `create_cluster_v2_serverless_authentication`'s rule).
fn serverless_body(row: &Row) -> Value {
    json!({
        "clusterName": row.name,
        "serverless": {
            "vpcConfigs": row
                .subnets
                .iter()
                .map(|subnets| json!({ "subnetIds": subnets }))
                .collect::<Vec<Value>>(),
            "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
        },
    })
}

/// The `Provisioned` body of a row, as the v2 create groups the members `CreateCluster`
/// carries at the top level.
fn provisioned_v2_body(row: &Row) -> Value {
    json!({
        "clusterName": row.name,
        "provisioned": {
            "kafkaVersion": row.kafka_version,
            "numberOfBrokerNodes": row.brokers,
            "brokerNodeGroupInfo": {
                "instanceType": "kafka.m5.large",
                "clientSubnets": row.subnets.first().cloned().unwrap_or_default(),
            },
        },
    })
}

/// Create the cluster `row` asks for and hand back the ARN its create answered with.
async fn create(emulator: &Emulator, row: &Row) -> String {
    let reply = match (row.kind, row.over_v2) {
        (Kind::Serverless, _) => {
            emulator
                .call_in(row.region, "POST", "/api/v2/clusters", Some(serverless_body(row)))
                .await
        }
        (Kind::Provisioned, true) => {
            emulator
                .call_in(
                    row.region,
                    "POST",
                    "/api/v2/clusters",
                    Some(provisioned_v2_body(row)),
                )
                .await
        }
        (Kind::Provisioned, false) => {
            let request = ClusterRequest {
                brokers: row.brokers,
                kafka_version: row.kafka_version,
                subnets: row.subnets.first().cloned().unwrap_or_default(),
                ..ClusterRequest::named(row.name)
            };
            emulator
                .call_in(row.region, "POST", "/v1/clusters", Some(request.to_json()))
                .await
        }
    };
    assert_eq!(
        reply.status, 200,
        "case {}: expected the create to return 200, got {} {}",
        row.case, reply.status, reply.body
    );
    reply
        .at("/clusterArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {}: the create reported no clusterArn: {}", row.case, reply.body))
        .to_string()
}

/// The ARN's tail split into the cluster's uuid and the type segment that follows it,
/// having first pinned the ARN's prefix: `arn:aws:kafka:<region>:<account>:cluster/<name>/`.
fn uuid_and_type_segment(case: &str, arn: &str, name: &str, region: &str) -> (String, String) {
    let prefix = format!("arn:aws:kafka:{region}:{ACCOUNT}:cluster/{name}/");
    assert!(
        arn.starts_with(&prefix),
        "case {case}: expected an ARN under {prefix:?}, got {arn:?}"
    );
    let tail = &arn[prefix.len()..];
    let (uuid, segment) = tail
        .rsplit_once('-')
        .unwrap_or_else(|| panic!("case {case}: the ARN tail {tail:?} carries no type segment"));
    // The id before the segment is a version-4 uuid, whichever kind the cluster is: the
    // type segment replaces the ordinal, not the id.
    let groups: Vec<&str> = uuid.split('-').collect();
    assert_eq!(
        groups.iter().map(|group| group.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12],
        "case {case}: expected a uuid-shaped cluster id, got {uuid:?} in {arn:?}"
    );
    assert!(
        groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "case {case}: expected a hexadecimal cluster id, got {uuid:?} in {arn:?}"
    );
    (uuid.to_string(), segment.to_string())
}

/// RULE: the segment a minted cluster ARN ends in is the serverless marker for a
/// serverless cluster and the cluster's decimal ordinal for a provisioned one — and it
/// agrees with the `ClusterType` the readers report for that same ARN.
#[tokio::test]
async fn a_cluster_arn_ends_in_the_segment_of_its_own_cluster_type() {
    // Interleaved on purpose: each create advances the registry's one ordinal, so a
    // serverless row sits between provisioned rows and vice versa.
    let rows = vec![
        Row {
            case: "serverless, one config with one subnet",
            kind: Kind::Serverless,
            name: "segment-sl-minimal",
            region: REGION,
            subnets: vec![vec!["subnet-0a1b2c3d"]],
            brokers: 0,
            kafka_version: "",
            over_v2: false,
        },
        Row {
            case: "provisioned over the v1 create",
            kind: Kind::Provisioned,
            name: "segment-pv-v1",
            region: REGION,
            subnets: vec![vec!["subnet-a", "subnet-b", "subnet-c"]],
            brokers: 3,
            kafka_version: "3.6.0",
            over_v2: false,
        },
        Row {
            case: "serverless, two configs with two and three subnets",
            kind: Kind::Serverless,
            name: "segment-sl-two-configs",
            region: REGION,
            subnets: vec![
                vec!["subnet-aaa", "subnet-bbb"],
                vec!["subnet-ccc", "subnet-ddd", "subnet-eee"],
            ],
            brokers: 0,
            kafka_version: "",
            over_v2: false,
        },
        Row {
            case: "provisioned over the v2 create",
            kind: Kind::Provisioned,
            name: "segment-pv-v2",
            region: REGION,
            subnets: vec![vec!["subnet-a"]],
            brokers: 1,
            kafka_version: "2.8.1",
            over_v2: true,
        },
        Row {
            case: "serverless in another region",
            kind: Kind::Serverless,
            name: "segment-sl-eu",
            region: "eu-central-1",
            subnets: vec![vec!["subnet-0f1e2d3c4b5a69780"]],
            brokers: 0,
            kafka_version: "",
            over_v2: false,
        },
        Row {
            case: "provisioned in another region",
            kind: Kind::Provisioned,
            name: "segment-pv-eu",
            region: "eu-central-1",
            subnets: vec![vec!["subnet-a", "subnet-b"]],
            brokers: 2,
            kafka_version: "3.6.0",
            over_v2: true,
        },
    ];

    let emulator = Emulator::new();
    let mut minted: Vec<(Row, String, String)> = Vec::new();
    for row in &rows {
        let arn = create(&emulator, row).await;
        let (uuid, segment) = uuid_and_type_segment(row.case, &arn, row.name, row.region);
        match row.kind {
            Kind::Serverless => assert_eq!(
                segment, SERVERLESS_SEGMENT,
                "case {}: a serverless cluster's ARN ends in {SERVERLESS_SEGMENT:?}, got \
                 {segment:?} in {arn:?}",
                row.case
            ),
            Kind::Provisioned => {
                let ordinal: u64 = segment.parse().unwrap_or_else(|_| {
                    panic!(
                        "case {}: a provisioned cluster's ARN ends in its decimal ordinal, got \
                         {segment:?} in {arn:?}",
                        row.case
                    )
                });
                assert!(
                    ordinal >= 1,
                    "case {}: expected a 1-based ordinal, got {ordinal} in {arn:?}",
                    row.case
                );
                assert_ne!(
                    segment, SERVERLESS_SEGMENT,
                    "case {}: a provisioned cluster must not carry the serverless segment, got \
                     {arn:?}",
                    row.case
                );
            }
        }
        minted.push((row.clone(), arn, uuid));
    }

    // The segment is checked against the registry's own answer, not against the request:
    // the V2 reader resolves each minted ARN and reports the type the segment names.
    for (row, arn, _) in &minted {
        let reply = emulator
            .call_in(
                row.region,
                "GET",
                &format!("/api/v2/clusters/{}", pct(arn)),
                None,
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {}: the minted ARN must resolve, got {} {}",
            row.case, reply.status, reply.body
        );
        assert_eq!(
            reply.at("/clusterInfo/clusterArn").as_str(),
            Some(arn.as_str()),
            "case {}: expected the reader to report the minted ARN, got {}",
            row.case,
            reply.at("/clusterInfo/clusterArn")
        );
        assert_eq!(
            reply.at("/clusterInfo/clusterType").as_str(),
            Some(row.kind.cluster_type()),
            "case {}: expected clusterType {}, got {}",
            row.case,
            row.kind.cluster_type(),
            reply.at("/clusterInfo/clusterType")
        );
    }

    // The segment identifies the KIND, so it repeats across clusters of one kind — which
    // is why the id before it is what keeps two clusters of the same kind apart.
    let serverless_uuids: Vec<&String> = minted
        .iter()
        .filter(|(row, _, _)| row.kind == Kind::Serverless)
        .map(|(_, _, uuid)| uuid)
        .collect();
    for (index, uuid) in serverless_uuids.iter().enumerate() {
        assert!(
            !serverless_uuids[index + 1..].contains(uuid),
            "every serverless cluster carries its own id, got {uuid:?} twice in \
             {serverless_uuids:?}"
        );
    }
    let arns: Vec<&String> = minted.iter().map(|(_, arn, _)| arn).collect();
    for (index, arn) in arns.iter().enumerate() {
        assert!(
            !arns[index + 1..].contains(arn),
            "every cluster carries its own ARN, got {arn:?} twice in {arns:?}"
        );
    }
}
