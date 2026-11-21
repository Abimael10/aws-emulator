// SPDX-License-Identifier: CC0-1.0

//! The SECURITY GROUPS a serverless Amazon MSK cluster reports are the ones its network
//! interfaces are really attached to — and when the create named none, that is the default
//! security group Amazon EC2 holds for the VPC. The second suite holds the same rule to the
//! one broker node group of a PROVISIONED cluster, the sibling input where the member is
//! optional too.
//!
//! RULE: the elastic network interfaces a cluster is reached through are created inside the
//! subnets each of its VPC configurations names, and an interface is always attached to at
//! least one security group — so a configuration that names groups is attached to exactly
//! those, and one that names none is attached to the default security group of the VPC its
//! subnets are in ("Your VPC automatically comes with a default security group", Amazon VPC
//! User Guide). Either way the attachment is what the cluster REPORTS: `DescribeClusterV2`
//! answers `ClusterInfo.Serverless.VpcConfigs[*].SecurityGroupIds` ("The IDs of the security
//! groups associated with the cluster",
//! `smithy_query(query="shape", shape="com.amazonaws.kafka#VpcConfig")`) present and
//! non-empty for every configuration, beside the `SubnetIds` it echoes, and both cluster
//! readers answer `BrokerNodeGroupInfo.SecurityGroups` ("The AWS security groups to
//! associate with the elastic network interfaces", same query on `BrokerNodeGroupInfo`) for
//! a provisioned cluster.
//!
//! This suite lives beside the router because the groups and the subnets are Amazon EC2's,
//! not Amazon MSK's: every VPC, subnet and security group below is minted by the real EC2
//! operations on the SAME engine that serves Amazon MSK, so every id in a request is one
//! EC2 handed out, and every group the cluster reports is checked against EC2's own read of
//! it — a group a cluster reports attached is a group EC2 holds, in the VPC the
//! configuration is placed in.
//!
//! The rows differ along what the rule generalises over: how many VPC configurations the
//! create names, how many subnets each names, how many security groups it names (none, the
//! empty list, one, two), which VPC each configuration is placed in, whether the two
//! configurations of one create differ in naming groups, and — for the provisioned suite —
//! which create surface the body goes over.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The name of the group every VPC comes with, which `DescribeSecurityGroups` selects on
/// through its `group-name` filter.
const DEFAULT_GROUP_NAME: &str = "default";

/// One emulator carrying Amazon EC2 and Amazon MSK over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form field of an ec2Query body.
fn field(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The form fields of one `Filters` entry at position `index` (counting from 1), as
/// ec2Query flattens a list into indexed keys.
fn filter(index: usize, name: &str, values: &[&str]) -> Vec<(String, String)> {
    let mut fields = vec![field(&format!("Filter.{index}.Name"), name)];
    for (position, value) in values.iter().enumerate() {
        fields.push(field(&format!("Filter.{index}.Value.{}", position + 1), value));
    }
    fields
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One answer from Amazon MSK: its HTTP status and its body as JSON.
struct Reply {
    status: u16,
    body: Value,
}

/// Call one Amazon MSK operation over its restJson1 wire.
async fn kafka(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("kafka.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("kafka"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// Percent-encode a URI component. An MSK ARN is an `@httpLabel`, so its `:` and `/` must
/// be encoded to keep it one path segment.
fn pct(value: &str) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(char::from(*byte));
        } else {
            // Infallible: `{:02X}` raises no formatting error, and a `String` is the one
            // `fmt::Write` that cannot fail for any other reason.
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// One VPC of the account, as EC2 minted it: the VPC, the subnets created in it, and the
/// id of the default security group it came with.
struct Vpc {
    id: String,
    subnets: Vec<String>,
    default_group: String,
}

/// Create a VPC with `zones.len()` subnets through EC2's own operations, and read back the
/// default security group it came with (through the filters `DescribeSecurityGroups`
/// documents, rather than assuming an id).
async fn ec2_vpc(
    router: &MultiServiceRouter,
    cidr_prefix: &str,
    zones: &[&str],
) -> Vpc {
    let created = ec2(
        router,
        "CreateVpc",
        &[field("CidrBlock", &format!("{cidr_prefix}.0.0/16"))],
    )
    .await;
    let id = element(&created, "vpcId");
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let answer = ec2(
            router,
            "CreateSubnet",
            &[
                field("VpcId", &id),
                field("CidrBlock", &format!("{cidr_prefix}.{}.0/24", index + 1)),
                field("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&answer, "subnetId"));
    }
    let mut params = Vec::new();
    params.extend(filter(1, "vpc-id", &[id.as_str()]));
    params.extend(filter(2, "group-name", &[DEFAULT_GROUP_NAME]));
    let groups = ec2(router, "DescribeSecurityGroups", &params).await;
    let default_group = element(&groups, "groupId");
    Vpc {
        id,
        subnets,
        default_group,
    }
}

/// Create a security group of this suite's own in `vpc_id` and answer its id — a group
/// EC2 really holds, so a row that names groups names ids EC2 handed out.
async fn ec2_group(router: &MultiServiceRouter, vpc_id: &str, name: &str) -> String {
    let created = ec2(
        router,
        "CreateSecurityGroup",
        &[
            field("GroupName", name),
            field("GroupDescription", "a group an MSK client is attached to"),
            field("VpcId", vpc_id),
        ],
    )
    .await;
    element(&created, "groupId")
}

/// The VPC Amazon EC2 reports the security group `group_id` in, read back through EC2's own
/// `DescribeSecurityGroups` — which is what makes a group a cluster reports a group that
/// EXISTS rather than an id nothing resolves.
async fn vpc_of_group(router: &MultiServiceRouter, group_id: &str) -> String {
    let answer = ec2(
        router,
        "DescribeSecurityGroups",
        &filter(1, "group-id", &[group_id]),
    )
    .await;
    assert_eq!(
        element(&answer, "groupId"),
        group_id,
        "EC2 must report the group {group_id} a cluster named, got {answer}"
    );
    element(&answer, "vpcId")
}

/// One VPC configuration a row's create carries: the subnets it names, the security groups
/// it names (`None` for a configuration that omits the member), and the groups the cluster
/// must then report for it.
struct Configuration {
    subnet_ids: Vec<String>,
    named_groups: Option<Vec<String>>,
    expected_groups: Vec<String>,
}

/// One row: its case name and the VPC configurations its create names.
struct Row {
    case: &'static str,
    configurations: Vec<Configuration>,
}

/// The `Serverless` body of a row, with the SASL/IAM client authentication MSK admits a
/// serverless cluster's clients through (a create that does not turn it on is refused).
fn serverless_body(name: &str, row: &Row) -> Value {
    let configs: Vec<Value> = row
        .configurations
        .iter()
        .map(|configuration| {
            let mut config = json!({ "subnetIds": configuration.subnet_ids });
            if let Some(groups) = &configuration.named_groups {
                config["securityGroupIds"] = json!(groups);
            }
            config
        })
        .collect();
    json!({
        "clusterName": name,
        "serverless": {
            "vpcConfigs": configs,
            "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
        },
    })
}

/// RULE: every VPC configuration of a serverless cluster reports the security groups its
/// interfaces are attached to — the ones the create named, or the default security group of
/// the VPC its subnets are in when the create named none — beside the subnets it echoes.
#[tokio::test]
async fn a_serverless_cluster_reports_the_security_groups_its_interfaces_are_attached_to() {
    let router = emulator();

    // Two VPCs, so a configuration's default group is the group of ITS OWN VPC rather than
    // whichever one the account happens to hold first.
    let first = ec2_vpc(&router, "10.90", &["us-west-2a", "us-west-2b", "us-west-2c"]).await;
    let second = ec2_vpc(&router, "10.91", &["us-west-2a"]).await;
    assert_ne!(
        first.default_group, second.default_group,
        "two VPCs come with two different default groups, got {} twice",
        first.default_group
    );
    let client_group = ec2_group(&router, &first.id, "msk-clients").await;
    let admin_group = ec2_group(&router, &first.id, "msk-admins").await;

    let rows = [
        Row {
            case: "one configuration, one subnet, no groups named",
            configurations: vec![Configuration {
                subnet_ids: vec![first.subnets[0].clone()],
                named_groups: None,
                expected_groups: vec![first.default_group.clone()],
            }],
        },
        Row {
            // Three subnets of ONE VPC are one interface's worth of groups, not three:
            // the default group of that VPC, once.
            case: "one configuration, three subnets of one VPC, no groups named",
            configurations: vec![Configuration {
                subnet_ids: first.subnets.clone(),
                named_groups: None,
                expected_groups: vec![first.default_group.clone()],
            }],
        },
        Row {
            case: "one configuration naming one group",
            configurations: vec![Configuration {
                subnet_ids: vec![first.subnets[1].clone()],
                named_groups: Some(vec![client_group.clone()]),
                expected_groups: vec![client_group.clone()],
            }],
        },
        Row {
            case: "one configuration naming two groups, in the order it named them",
            configurations: vec![Configuration {
                subnet_ids: vec![first.subnets[0].clone(), first.subnets[2].clone()],
                named_groups: Some(vec![admin_group.clone(), client_group.clone()]),
                expected_groups: vec![admin_group.clone(), client_group.clone()],
            }],
        },
        Row {
            // An interface is always attached to a group, so an EMPTY list names none of
            // its own exactly as an absent member does.
            case: "one configuration naming the empty list",
            configurations: vec![Configuration {
                subnet_ids: vec![second.subnets[0].clone()],
                named_groups: Some(Vec::new()),
                expected_groups: vec![second.default_group.clone()],
            }],
        },
        Row {
            case: "two configurations in two VPCs, neither naming groups",
            configurations: vec![
                Configuration {
                    subnet_ids: vec![first.subnets[0].clone(), first.subnets[1].clone()],
                    named_groups: None,
                    expected_groups: vec![first.default_group.clone()],
                },
                Configuration {
                    subnet_ids: vec![second.subnets[0].clone()],
                    named_groups: None,
                    expected_groups: vec![second.default_group.clone()],
                },
            ],
        },
        Row {
            case: "two configurations, only the first naming a group",
            configurations: vec![
                Configuration {
                    subnet_ids: vec![first.subnets[2].clone()],
                    named_groups: Some(vec![client_group.clone()]),
                    expected_groups: vec![client_group.clone()],
                },
                Configuration {
                    subnet_ids: vec![second.subnets[0].clone()],
                    named_groups: None,
                    expected_groups: vec![second.default_group.clone()],
                },
            ],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // Each row stands a cluster of its own up, so the registry's ordinals advance
        // between rows and no row's answer can come from another's.
        let name = format!("serverless-groups-{index}");
        let created = kafka(
            &router,
            "POST",
            "/api/v2/clusters",
            Some(serverless_body(&name, row)),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected CreateClusterV2 to return 200, got {} {}",
            row.case, created.status, created.body
        );
        let arn = created
            .body
            .get("clusterArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {:?}: the create reported no clusterArn: {}", row.case, created.body)
            })
            .to_string();

        let described = kafka(
            &router,
            "GET",
            &format!("/api/v2/clusters/{}", pct(&arn)),
            None,
        )
        .await;
        assert_eq!(
            described.status, 200,
            "case {:?}: expected DescribeClusterV2 to return 200, got {} {}",
            row.case, described.status, described.body
        );
        let reported = described
            .body
            .pointer("/clusterInfo/serverless/vpcConfigs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported.len(),
            row.configurations.len(),
            "case {:?}: expected {} VPC configuration(s) back, got {}",
            row.case,
            row.configurations.len(),
            described.body
        );

        for (position, configuration) in row.configurations.iter().enumerate() {
            let answered = &reported[position];
            assert_eq!(
                answered.get("subnetIds").cloned().unwrap_or(Value::Null),
                json!(configuration.subnet_ids),
                "case {:?}, configuration {position}: expected the submitted subnets back, got \
                 {answered}",
                row.case
            );
            let groups: Vec<String> = answered
                .get("securityGroupIds")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|group| group.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                groups, configuration.expected_groups,
                "case {:?}, configuration {position}: expected the attached groups {:?}, got \
                 {answered}",
                row.case, configuration.expected_groups
            );
            assert!(
                !groups.is_empty(),
                "case {:?}, configuration {position}: an interface is always attached to a \
                 group, got {answered}",
                row.case
            );

            // Every reported group is one Amazon EC2 holds, in the VPC the configuration's
            // subnets are in: the cluster reports an attachment that exists rather than an
            // id nothing resolves.
            let expected_vpc = if configuration.subnet_ids.iter().all(|subnet| {
                second.subnets.contains(subnet)
            }) {
                second.id.as_str()
            } else {
                first.id.as_str()
            };
            for group in &groups {
                let held_by = vpc_of_group(&router, group).await;
                assert_eq!(
                    held_by, expected_vpc,
                    "case {:?}, configuration {position}: EC2 must hold {group} in \
                     {expected_vpc}, got {held_by}",
                    row.case
                );
            }
        }
    }
}

/// One row of the provisioned table: the create surface it goes over, the groups its
/// `BrokerNodeGroupInfo` names (`None` for a body that omits the member), and the groups
/// the cluster must then report.
struct BrokerGroupRow {
    case: &'static str,
    /// Whether the create goes over `CreateClusterV2`'s `Provisioned` body rather than the
    /// v1 `CreateCluster`: both carry the same configuration, so both must store the same
    /// attachment.
    over_v2: bool,
    named_groups: Option<Vec<String>>,
    expected_groups: Vec<String>,
}

/// The create body of a provisioned row, in the shape the surface it names carries.
fn provisioned_body(name: &str, row: &BrokerGroupRow, subnets: &[String]) -> Value {
    let mut broker_group = json!({
        "instanceType": "kafka.m5.large",
        "clientSubnets": subnets,
    });
    if let Some(groups) = &row.named_groups {
        broker_group["securityGroups"] = json!(groups);
    }
    let provisioned = json!({
        // A version Amazon MSK still takes for a NEW cluster, since the create has
        // to be admitted before the attachment this row is about is stored.
        "kafkaVersion": "3.9.x",
        "numberOfBrokerNodes": subnets.len(),
        "brokerNodeGroupInfo": broker_group,
    });
    match row.over_v2 {
        true => json!({ "clusterName": name, "provisioned": provisioned }),
        false => {
            let mut body = provisioned;
            body["clusterName"] = json!(name);
            body
        }
    }
}

/// RULE: the same attachment holds for the one broker node group of a PROVISIONED cluster —
/// `BrokerNodeGroupInfo$SecurityGroups` reports the groups the create named, or the default
/// security group of the VPC its client subnets are in when the create named none — and both
/// readers of the cluster report that one stored attachment.
#[tokio::test]
async fn a_provisioned_clusters_broker_node_group_reports_the_groups_it_is_attached_to() {
    let router = emulator();
    let network = ec2_vpc(&router, "10.92", &["us-west-2a", "us-west-2b"]).await;
    let client_group = ec2_group(&router, &network.id, "msk-broker-clients").await;
    let admin_group = ec2_group(&router, &network.id, "msk-broker-admins").await;

    // The rows differ along the create surface and along how many groups the body names:
    // none at all, the empty list, one, two.
    let rows = [
        BrokerGroupRow {
            case: "the v1 create naming no groups",
            over_v2: false,
            named_groups: None,
            expected_groups: vec![network.default_group.clone()],
        },
        BrokerGroupRow {
            case: "the v2 provisioned body naming no groups",
            over_v2: true,
            named_groups: None,
            expected_groups: vec![network.default_group.clone()],
        },
        BrokerGroupRow {
            case: "the v1 create naming the empty list",
            over_v2: false,
            named_groups: Some(Vec::new()),
            expected_groups: vec![network.default_group.clone()],
        },
        BrokerGroupRow {
            case: "the v1 create naming one group",
            over_v2: false,
            named_groups: Some(vec![client_group.clone()]),
            expected_groups: vec![client_group.clone()],
        },
        BrokerGroupRow {
            case: "the v2 provisioned body naming two groups, in the order it named them",
            over_v2: true,
            named_groups: Some(vec![admin_group.clone(), client_group.clone()]),
            expected_groups: vec![admin_group.clone(), client_group.clone()],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("broker-groups-{index}");
        let uri = match row.over_v2 {
            true => "/api/v2/clusters",
            false => "/v1/clusters",
        };
        let created = kafka(
            &router,
            "POST",
            uri,
            Some(provisioned_body(&name, row, &network.subnets)),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected the create to return 200, got {} {}",
            row.case, created.status, created.body
        );
        let arn = created
            .body
            .get("clusterArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {:?}: the create reported no clusterArn: {}", row.case, created.body)
            })
            .to_string();

        // Both readers report the one stored attachment: the v1 reader at the top of
        // `ClusterInfo`, the V2 reader under the kind the cluster is.
        for (reader, uri, pointer) in [
            (
                "DescribeCluster",
                format!("/v1/clusters/{}", pct(&arn)),
                "/clusterInfo/brokerNodeGroupInfo/securityGroups",
            ),
            (
                "DescribeClusterV2",
                format!("/api/v2/clusters/{}", pct(&arn)),
                "/clusterInfo/provisioned/brokerNodeGroupInfo/securityGroups",
            ),
        ] {
            let described = kafka(&router, "GET", &uri, None).await;
            assert_eq!(
                described.status, 200,
                "case {:?}: expected {reader} to return 200, got {} {}",
                row.case, described.status, described.body
            );
            let groups: Vec<String> = described
                .body
                .pointer(pointer)
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|group| group.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                groups, row.expected_groups,
                "case {:?}: expected {reader} to report the attached groups {:?}, got {}",
                row.case, row.expected_groups, described.body
            );
            assert!(
                !groups.is_empty(),
                "case {:?}: a broker interface is always attached to a group, got {}",
                row.case,
                described.body
            );
            for group in &groups {
                let held_by = vpc_of_group(&router, group).await;
                assert_eq!(
                    held_by, network.id,
                    "case {:?}: EC2 must hold {group} in {}, got {held_by}",
                    row.case, network.id
                );
            }
        }
    }
}
