// SPDX-License-Identifier: CC0-1.0

//! The Amazon EKS workloads a stack declares ON a cluster — an `AWS::EKS::Addon` and an
//! `AWS::EKS::Nodegroup` — read back through AMAZON EKS's own wire.
//!
//! RULE: a resource type CloudFormation provisions is created through the owning service's
//! own API, with every call that resource's declaration asks for, its creation held open
//! until that service reports the resource ready — so a stack that reports `CREATE_COMPLETE`
//! names a resource that service independently serves, the values the type publishes are the
//! ones that service reported, and a declaration the owning service cannot satisfy FAILS the
//! resource and the stack rather than completing it with nothing created.
//!
//! For these two types the resource is a CHILD of a cluster: every operation of that service
//! on one addresses it by the cluster and the resource's own name (the two `@httpLabel`s of
//! `DescribeAddon` and `DescribeNodegroup`), and each create is asynchronous — an add-on is
//! `CREATING` while its Kubernetes resources are applied and a node group while its
//! instances come up — so the stack waits for `ACTIVE` before it reports the resource
//! created.
//!
//! The rows differ along what the rule generalises over: which add-on of the published
//! catalog the template installs and whether its declaration carries a version, configuration
//! values, a conflict resolution or tags; and for a node group, whether its name is declared
//! or generated, how many subnets it launches into, and which of the scaling, AMI, capacity,
//! instance-type, disk, label, taint, version and update settings the declaration states.
//! Every expected value is computed from what the row declares or from what Amazon EKS
//! answered.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the Amazon EKS calls over that
//! service's own `restJson1` wire, the Auto Scaling read over that service's query wire — so
//! what is asserted is what a client that deploys the template and then calls those services
//! observes.

use serde_json::{json, Value};
use service_com_amazonaws_eks_awswesleyfrontend::state::DEFAULT_KUBERNETES_VERSION;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    create_stack, router, settled_stack, settled_stack_status, stack_event_reasons, xml_text,
    xml_texts, REGION,
};
use crate::stack_lifecycle::{autoscaling, ec2, output_value};

/// The two resource types under test.
const ADDON: &str = "AWS::EKS::Addon";
const NODEGROUP: &str = "AWS::EKS::Nodegroup";

/// The `StackStatus` a completed create reports, and the one a create that could not be
/// completed settles at once CloudFormation has rolled it back.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `AddonStatus` an installed add-on is usable in, and the `NodegroupStatus` a managed
/// node group is usable in: both are `ACTIVE`.
const ACTIVE: &str = "ACTIVE";

/// The status Amazon EKS answers for a resource it does not hold (the modelled 404 of
/// `DescribeAddon` and `DescribeNodegroup`).
const NOT_FOUND_STATUS: u16 = 404;

/// The logical ids the templates below declare their resources under.
const VPC_ID: &str = "Net";
const SUBNET_IDS: [&str; 3] = ["SubnetA", "SubnetB", "SubnetC"];
const CLUSTER_ID: &str = "Cluster";
const ADDON_ID: &str = "Addon";
const NODEGROUP_ID: &str = "NodeGroup";

/// The Availability Zones the subnets are created in, in declaration order: a cluster's
/// control plane is placed across the zones of the subnets it names, so a row that declares
/// three subnets spreads them.
const ZONES: [&str; 3] = ["us-east-1a", "us-east-1b", "us-east-1c"];

/// One Amazon EKS call over the `restJson1` wire its model declares, on the method and URI
/// that operation's own `@http` binding names, signed for `eks` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
async fn eks(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://eks.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("eks.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/eks/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Amazon EKS request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one add-on of one cluster.
async fn describe_addon(router: &MultiServiceRouter, cluster: &str, addon: &str) -> (u16, Value) {
    eks(
        router,
        "GET",
        &format!("/clusters/{cluster}/addons/{addon}"),
        None,
    )
    .await
}

/// That service's own read of one managed node group of one cluster.
async fn describe_nodegroup(
    router: &MultiServiceRouter,
    cluster: &str,
    nodegroup: &str,
) -> (u16, Value) {
    eks(
        router,
        "GET",
        &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
        None,
    )
    .await
}

/// The names a list read of one cluster's children reports, under the member that read
/// answers them in.
async fn listed(router: &MultiServiceRouter, uri: &str, member: &str) -> Vec<String> {
    let (_, body) = eks(router, "GET", uri, None).await;
    strings_of(&body[member])
}

/// The strings of a JSON array, or nothing for an answer that carries none.
fn strings_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A role ARN of the account this emulator serves, which is what a cluster's `RoleArn` and a
/// node group's `NodeRole` are.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The network half of a template: the VPC and the subnets, each created in EC2 by the stack
/// itself, so a cluster's and a node group's subnet ids are ids EC2 assigned rather than
/// values a template invented.
fn network_resources(subnet_count: usize) -> serde_json::Map<String, Value> {
    let mut resources = serde_json::Map::new();
    resources.insert(
        VPC_ID.to_string(),
        json!({
            "Type": "AWS::EC2::VPC",
            "Properties": { "CidrBlock": "10.91.0.0/16" }
        }),
    );
    for index in 0..subnet_count {
        resources.insert(
            SUBNET_IDS[index].to_string(),
            json!({
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": VPC_ID },
                    "CidrBlock": format!("10.91.{}.0/24", index + 1),
                    "AvailabilityZone": ZONES[index]
                }
            }),
        );
    }
    resources
}

/// The references a declaration makes to the first `count` subnets of a template.
fn subnet_refs(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({ "Ref": SUBNET_IDS[index] }))
            .collect(),
    )
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(router, stack_name, template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    settled_stack(case, router, REGION, stack_name).await
}

/// The cluster every row's workload is installed on, declared with the network it sits in and
/// publishing its name and its subnet ids so the per-row stacks below can name them.
fn host_template(subnet_count: usize) -> String {
    let mut resources = network_resources(subnet_count);
    resources.insert(
        CLUSTER_ID.to_string(),
        json!({
            "Type": "AWS::EKS::Cluster",
            "Properties": {
                "RoleArn": role_arn("eksClusterRole"),
                "ResourcesVpcConfig": { "SubnetIds": subnet_refs(subnet_count) }
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert(
        "ClusterName".to_string(),
        json!({ "Value": { "Ref": CLUSTER_ID } }),
    );
    for logical_id in SUBNET_IDS.iter().take(subnet_count) {
        outputs.insert(
            (*logical_id).to_string(),
            json!({ "Value": { "Ref": logical_id } }),
        );
    }
    json!({
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs)
    })
    .to_string()
}

/// Deploy the host stack and answer the cluster name and the subnet ids it published.
async fn deploy_host(
    router: &MultiServiceRouter,
    stack_name: &str,
    subnet_count: usize,
) -> (String, Vec<String>) {
    let case = "the host cluster";
    let body = deployed(router, case, stack_name, &host_template(subnet_count)).await;
    let cluster_name = output_value(&body, "ClusterName")
        .unwrap_or_else(|| panic!("setup: no ClusterName output in {body}"));
    let subnets = (0..subnet_count)
        .map(|index| {
            output_value(&body, SUBNET_IDS[index])
                .unwrap_or_else(|| panic!("setup: no {} output in {body}", SUBNET_IDS[index]))
        })
        .collect();
    (cluster_name, subnets)
}

/// The version Amazon EKS's own catalog publishes for `addon_name` on a cluster of
/// `kubernetes_version` — read out of `DescribeAddonVersions` rather than written here, so a
/// row that declares a version declares one that service really offers.
async fn published_addon_version(
    router: &MultiServiceRouter,
    addon_name: &str,
    kubernetes_version: &str,
) -> String {
    let (status, body) = eks(
        router,
        "GET",
        &format!(
            "/addons/supported-versions?addonName={addon_name}\
             &kubernetesVersion={kubernetes_version}"
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected the add-on catalog to answer, got {status} with body {body}"
    );
    body["addons"][0]["addonVersions"][0]["addonVersion"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup: no published version for {addon_name:?} in {body}"))
}

/// One row of the add-on table: which add-on the template installs and what its declaration
/// states beside the cluster it is installed on.
struct AddonRow {
    case: &'static str,
    /// The add-on's name, which is a name the Amazon EKS add-on catalog publishes.
    addon_name: &'static str,
    /// The `ResolveConflicts` the declaration states, if any.
    resolve_conflicts: Option<&'static str>,
    /// Whether the declaration states the version the catalog publishes for the cluster's
    /// Kubernetes version.
    declare_version: bool,
    /// The `ConfigurationValues` document the declaration states, if any — a JSON object of
    /// keys the add-on's own configuration schema publishes.
    configuration_values: Option<&'static str>,
    /// The tags the declaration asks for.
    tags: &'static [(&'static str, &'static str)],
}

/// Three add-ons the catalog publishes, each installed with a different part of the
/// declaration stated. (Source for the names and for every version: `DescribeAddonVersions`,
/// which publishes the add-ons a cluster of a Kubernetes version can install — the suite
/// reads a version out of that same answer rather than writing one here.)
const ADDON_ROWS: &[AddonRow] = &[
    AddonRow {
        case: "vpc-cni with a conflict resolution and nothing else declared",
        addon_name: "vpc-cni",
        resolve_conflicts: Some("OVERWRITE"),
        declare_version: false,
        configuration_values: None,
        tags: &[],
    },
    AddonRow {
        case: "kube-proxy with configuration values and two tags",
        addon_name: "kube-proxy",
        resolve_conflicts: None,
        declare_version: false,
        configuration_values: Some("{\"tolerations\":[]}"),
        tags: &[("team", "platform"), ("env", "test")],
    },
    AddonRow {
        case: "the pod identity agent at a published version, with one tag",
        addon_name: "eks-pod-identity-agent",
        resolve_conflicts: Some("NONE"),
        declare_version: true,
        configuration_values: None,
        tags: &[("team", "platform")],
    },
];

/// The template an add-on row deploys: the add-on alone, on the cluster the host stack
/// created, with its name and the ARN Amazon EKS minted as the stack's outputs.
fn addon_template(row: &AddonRow, cluster_name: &str, version: Option<&str>) -> String {
    let mut properties = serde_json::Map::new();
    properties.insert("ClusterName".to_string(), json!(cluster_name));
    properties.insert("AddonName".to_string(), json!(row.addon_name));
    if let Some(resolve_conflicts) = row.resolve_conflicts {
        properties.insert("ResolveConflicts".to_string(), json!(resolve_conflicts));
    }
    if let Some(version) = version {
        properties.insert("AddonVersion".to_string(), json!(version));
    }
    if let Some(configuration) = row.configuration_values {
        properties.insert("ConfigurationValues".to_string(), json!(configuration));
    }
    if !row.tags.is_empty() {
        properties.insert(
            "Tags".to_string(),
            Value::Array(
                row.tags
                    .iter()
                    .map(|(key, value)| json!({ "Key": key, "Value": value }))
                    .collect(),
            ),
        );
    }
    json!({
        "Resources": {
            ADDON_ID: { "Type": ADDON, "Properties": Value::Object(properties) }
        },
        "Outputs": {
            "AddonName": { "Value": { "Ref": ADDON_ID } },
            "AddonArn": { "Value": { "Fn::GetAtt": [ADDON_ID, "Arn"] } }
        }
    })
    .to_string()
}

/// Every add-on row: the stack completes once Amazon EKS reports the add-on `ACTIVE`, the
/// add-on is independently readable through `DescribeAddon` with a resolved version, listed
/// by `ListAddons`, and the ARN the stack publishes is the one that service minted.
#[tokio::test]
async fn a_stack_installs_the_eks_addon_its_template_declares() {
    let router = router();
    let (cluster_name, _subnets) = deploy_host(&router, "eks-addon-host", 2).await;

    for (index, row) in ADDON_ROWS.iter().enumerate() {
        let case = row.case;
        let declared_version = if row.declare_version {
            Some(published_addon_version(&router, row.addon_name, DEFAULT_KUBERNETES_VERSION).await)
        } else {
            None
        };
        let stack_name = format!("eks-addon-{index}");
        let body = deployed(
            &router,
            case,
            &stack_name,
            &addon_template(row, &cluster_name, declared_version.as_deref()),
        )
        .await;

        // What the stack publishes for the resource: the add-on's own name.
        assert_eq!(
            output_value(&body, "AddonName").as_deref(),
            Some(row.addon_name),
            "case {case:?}: expected Ref to report the declared add-on name, got {body}"
        );

        // The add-on is readable through the EKS API itself, independently of the stack.
        let (status, read) = describe_addon(&router, &cluster_name, row.addon_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the declared add-on to be readable, got {status} with \
             body {read}"
        );
        let addon = &read["addon"];
        assert_eq!(
            addon["status"],
            json!(ACTIVE),
            "case {case:?}: expected the add-on {ACTIVE} once the stack completed, got {read}"
        );
        let addon_version = addon["addonVersion"].as_str().unwrap_or_default();
        assert!(
            !addon_version.is_empty(),
            "case {case:?}: expected a resolved addonVersion, got {read}"
        );
        if let Some(declared) = declared_version.as_deref() {
            assert_eq!(
                addon_version, declared,
                "case {case:?}: expected the declared version on the add-on, got {read}"
            );
        }
        assert_eq!(
            addon["clusterName"],
            json!(cluster_name),
            "case {case:?}: expected the add-on on the declared cluster, got {read}"
        );
        if let Some(configuration) = row.configuration_values {
            assert_eq!(
                addon["configurationValues"],
                json!(configuration),
                "case {case:?}: expected the declared configuration values, got {read}"
            );
        }
        for (key, value) in row.tags {
            assert_eq!(
                addon["tags"][*key],
                json!(value),
                "case {case:?}: expected the declared tag {key:?}={value:?} on the add-on, \
                 got {read}"
            );
        }
        assert_eq!(
            addon["tags"]["aws:cloudformation:stack-name"],
            json!(stack_name),
            "case {case:?}: expected the propagated stack-name tag, got {read}"
        );

        // The add-on the stack named is one that service lists for the cluster.
        let names = listed(
            &router,
            &format!("/clusters/{cluster_name}/addons"),
            "addons",
        )
        .await;
        assert!(
            names.contains(&row.addon_name.to_string()),
            "case {case:?}: expected {:?} among the listed add-ons, got {names:?}",
            row.addon_name
        );

        // And the attribute the stack publishes is the ARN that service minted.
        assert_eq!(
            output_value(&body, "AddonArn").map(Value::from),
            Some(addon["addonArn"].clone()),
            "case {case:?}: expected the stack to report the add-on's own ARN, got {body}"
        );
    }
}

/// One row of the node group table: what the declaration states beside the cluster, the
/// subnets and the node role every row carries.
struct NodegroupRow {
    case: &'static str,
    /// The group's `NodegroupName`, or `None` where the template declares none and
    /// CloudFormation generates it.
    declared_name: Option<&'static str>,
    /// How many of the host stack's subnets the group launches nodes into.
    subnet_count: usize,
    /// The `ScalingConfig` sizes, as `(MinSize, DesiredSize, MaxSize)` — all three or none,
    /// which is what that configuration requires of a create.
    scaling: Option<(i64, i64, i64)>,
    /// The `AmiType` the declaration states, if any.
    ami_type: Option<&'static str>,
    /// The `CapacityType` the declaration states, if any.
    capacity_type: Option<&'static str>,
    /// The instance types the declaration states.
    instance_types: &'static [&'static str],
    /// The root disk size in GiB the declaration states, if any.
    disk_size: Option<i64>,
    /// The Kubernetes labels the declaration applies to the nodes.
    labels: &'static [(&'static str, &'static str)],
    /// The taints the declaration applies, as `(Key, Value, Effect)`.
    taints: &'static [(&'static str, &'static str, &'static str)],
    /// Whether the declaration states the group's `Version`, which must be the cluster's.
    declare_version: bool,
    /// The `UpdateConfig`'s `MaxUnavailable`, if the declaration states one.
    max_unavailable: Option<i64>,
    /// The tags the declaration asks for.
    tags: &'static [(&'static str, &'static str)],
}

const NODEGROUP_ROWS: &[NodegroupRow] = &[
    NodegroupRow {
        case: "no name declared, two subnets, nothing else stated",
        declared_name: None,
        subnet_count: 2,
        scaling: None,
        ami_type: None,
        capacity_type: None,
        instance_types: &[],
        disk_size: None,
        labels: &[],
        taints: &[],
        declare_version: false,
        max_unavailable: None,
        tags: &[],
    },
    NodegroupRow {
        case: "a named group with sizes, an AMI type, an instance type, a disk, a label and \
               a tag",
        declared_name: Some("stack-nodes-alpha"),
        subnet_count: 2,
        scaling: Some((1, 2, 3)),
        // The AMI type and the capacity type are values the model's own enums publish
        // (`AMITypes`, `CapacityTypes`).
        ami_type: Some("AL2023_x86_64_STANDARD"),
        capacity_type: Some("ON_DEMAND"),
        instance_types: &["t3.medium"],
        disk_size: Some(30),
        labels: &[("role", "worker")],
        taints: &[],
        declare_version: false,
        max_unavailable: None,
        tags: &[("team", "platform")],
    },
    NodegroupRow {
        case: "three subnets, spot capacity, two instance types, a taint, the cluster's \
               version and an update configuration",
        declared_name: Some("stack-nodes-gamma"),
        subnet_count: 3,
        scaling: Some((0, 1, 4)),
        ami_type: Some("AL2023_ARM_64_STANDARD"),
        capacity_type: Some("SPOT"),
        instance_types: &["t4g.small", "t4g.medium"],
        disk_size: None,
        labels: &[("role", "batch"), ("tier", "spot")],
        // `TaintEffect`'s own values.
        taints: &[("dedicated", "batch", "NO_SCHEDULE")],
        declare_version: true,
        max_unavailable: Some(1),
        tags: &[],
    },
];

/// The node group properties a row declares, against the cluster and the subnets the host
/// stack published.
fn nodegroup_properties(
    row: &NodegroupRow,
    cluster_name: &str,
    subnets: &[String],
) -> serde_json::Map<String, Value> {
    let mut properties = serde_json::Map::new();
    properties.insert("ClusterName".to_string(), json!(cluster_name));
    if let Some(name) = row.declared_name {
        properties.insert("NodegroupName".to_string(), json!(name));
    }
    properties.insert("NodeRole".to_string(), json!(role_arn("eksNodeRole")));
    properties.insert(
        "Subnets".to_string(),
        Value::Array(
            subnets
                .iter()
                .take(row.subnet_count)
                .map(|subnet| json!(subnet))
                .collect(),
        ),
    );
    if let Some((min, desired, max)) = row.scaling {
        properties.insert(
            "ScalingConfig".to_string(),
            json!({ "MinSize": min, "DesiredSize": desired, "MaxSize": max }),
        );
    }
    if let Some(ami_type) = row.ami_type {
        properties.insert("AmiType".to_string(), json!(ami_type));
    }
    if let Some(capacity_type) = row.capacity_type {
        properties.insert("CapacityType".to_string(), json!(capacity_type));
    }
    if !row.instance_types.is_empty() {
        properties.insert("InstanceTypes".to_string(), json!(row.instance_types));
    }
    if let Some(disk_size) = row.disk_size {
        properties.insert("DiskSize".to_string(), json!(disk_size));
    }
    if !row.labels.is_empty() {
        properties.insert("Labels".to_string(), map_of(row.labels));
    }
    if !row.taints.is_empty() {
        properties.insert(
            "Taints".to_string(),
            Value::Array(
                row.taints
                    .iter()
                    .map(|(key, value, effect)| {
                        json!({ "Key": key, "Value": value, "Effect": effect })
                    })
                    .collect(),
            ),
        );
    }
    if row.declare_version {
        properties.insert("Version".to_string(), json!(DEFAULT_KUBERNETES_VERSION));
    }
    if let Some(max_unavailable) = row.max_unavailable {
        properties.insert(
            "UpdateConfig".to_string(),
            json!({ "MaxUnavailable": max_unavailable }),
        );
    }
    if !row.tags.is_empty() {
        properties.insert("Tags".to_string(), map_of(row.tags));
    }
    properties
}

/// One declared MAP property, which is how `AWS::EKS::Nodegroup` spells its labels and its
/// tags.
fn map_of(pairs: &[(&str, &str)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), json!(value));
    }
    Value::Object(map)
}

/// The template a node group row deploys: the group alone, on the cluster and in the subnets
/// the host stack created, with its name and the ARN Amazon EKS minted as the outputs.
fn nodegroup_template(row: &NodegroupRow, cluster_name: &str, subnets: &[String]) -> String {
    let properties = nodegroup_properties(row, cluster_name, subnets);
    json!({
        "Resources": {
            NODEGROUP_ID: { "Type": NODEGROUP, "Properties": Value::Object(properties) }
        },
        "Outputs": {
            "NodegroupName": { "Value": { "Ref": NODEGROUP_ID } },
            "NodegroupArn": { "Value": { "Fn::GetAtt": [NODEGROUP_ID, "Arn"] } }
        }
    })
    .to_string()
}

/// Every node group row: the stack completes once Amazon EKS reports the group `ACTIVE`, the
/// group is independently readable through `DescribeNodegroup` with the configuration the
/// template gave it, listed by `ListNodegroups`, backed by an Auto Scaling group that service
/// really holds, and the ARN the stack publishes is the one Amazon EKS minted.
#[tokio::test]
async fn a_stack_creates_the_eks_node_group_its_template_declares() {
    let router = router();
    let (cluster_name, subnets) = deploy_host(&router, "eks-nodegroup-host", 3).await;

    for (index, row) in NODEGROUP_ROWS.iter().enumerate() {
        let case = row.case;
        let stack_name = format!("eks-nodegroup-{index}");
        let body = deployed(
            &router,
            case,
            &stack_name,
            &nodegroup_template(row, &cluster_name, &subnets),
        )
        .await;

        let nodegroup_name = output_value(&body, "NodegroupName")
            .unwrap_or_else(|| panic!("case {case:?}: no NodegroupName output in {body}"));
        if let Some(declared) = row.declared_name {
            assert_eq!(
                nodegroup_name, declared,
                "case {case:?}: expected Ref to report the declared name, got {body}"
            );
        } else {
            assert!(
                nodegroup_name.contains(&stack_name),
                "case {case:?}: expected a generated name naming its stack, got \
                 {nodegroup_name:?}"
            );
        }

        // The node group is readable through the EKS API itself, independently of the stack.
        let (status, read) = describe_nodegroup(&router, &cluster_name, &nodegroup_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the declared node group to be readable, got {status} \
             with body {read}"
        );
        let nodegroup = &read["nodegroup"];
        assert_eq!(
            nodegroup["status"],
            json!(ACTIVE),
            "case {case:?}: expected the node group {ACTIVE} once the stack completed, got \
             {read}"
        );
        assert_eq!(
            nodegroup["clusterName"],
            json!(cluster_name),
            "case {case:?}: expected the group under the declared cluster, got {read}"
        );
        assert_eq!(
            nodegroup["nodeRole"],
            json!(role_arn("eksNodeRole")),
            "case {case:?}: expected the declared node role, got {read}"
        );
        let declared_subnets: Vec<String> =
            subnets.iter().take(row.subnet_count).cloned().collect();
        assert_eq!(
            strings_of(&nodegroup["subnets"]),
            declared_subnets,
            "case {case:?}: expected the declared subnets, got {read}"
        );
        if let Some((min, desired, max)) = row.scaling {
            for (member, expected) in [
                ("minSize", min),
                ("desiredSize", desired),
                ("maxSize", max),
            ] {
                assert_eq!(
                    nodegroup["scalingConfig"][member],
                    json!(expected),
                    "case {case:?}: expected scalingConfig {member} {expected}, got {read}"
                );
            }
        }
        if let Some(ami_type) = row.ami_type {
            assert_eq!(
                nodegroup["amiType"],
                json!(ami_type),
                "case {case:?}: expected the declared AMI type, got {read}"
            );
        }
        if let Some(capacity_type) = row.capacity_type {
            assert_eq!(
                nodegroup["capacityType"],
                json!(capacity_type),
                "case {case:?}: expected the declared capacity type, got {read}"
            );
        }
        if !row.instance_types.is_empty() {
            assert_eq!(
                strings_of(&nodegroup["instanceTypes"]),
                row.instance_types
                    .iter()
                    .map(|instance_type| (*instance_type).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?}: expected the declared instance types, got {read}"
            );
        }
        if let Some(disk_size) = row.disk_size {
            assert_eq!(
                nodegroup["diskSize"],
                json!(disk_size),
                "case {case:?}: expected the declared disk size, got {read}"
            );
        }
        for (key, value) in row.labels {
            assert_eq!(
                nodegroup["labels"][*key],
                json!(value),
                "case {case:?}: expected the declared label {key:?}={value:?}, got {read}"
            );
        }
        for (key, value, effect) in row.taints {
            let reported = nodegroup["taints"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .find(|taint| taint["key"] == json!(key))
                .cloned();
            assert_eq!(
                reported,
                Some(json!({ "key": key, "value": value, "effect": effect })),
                "case {case:?}: expected the declared taint {key:?}, got {read}"
            );
        }
        if row.declare_version {
            assert_eq!(
                nodegroup["version"],
                json!(DEFAULT_KUBERNETES_VERSION),
                "case {case:?}: expected the declared Kubernetes version, got {read}"
            );
        }
        if let Some(max_unavailable) = row.max_unavailable {
            assert_eq!(
                nodegroup["updateConfig"]["maxUnavailable"],
                json!(max_unavailable),
                "case {case:?}: expected the declared update configuration, got {read}"
            );
        }
        for (key, value) in row.tags {
            assert_eq!(
                nodegroup["tags"][*key],
                json!(value),
                "case {case:?}: expected the declared tag {key:?}={value:?}, got {read}"
            );
        }
        assert_eq!(
            nodegroup["tags"]["aws:cloudformation:stack-name"],
            json!(stack_name),
            "case {case:?}: expected the propagated stack-name tag, got {read}"
        );

        // The group the stack named is one that service lists for the cluster.
        let names = listed(
            &router,
            &format!("/clusters/{cluster_name}/node-groups"),
            "nodegroups",
        )
        .await;
        assert!(
            names.contains(&nodegroup_name),
            "case {case:?}: expected {nodegroup_name:?} among the listed node groups, got \
             {names:?}"
        );

        // The capacity the group reports really exists in the service that owns it: the Auto
        // Scaling group it names is one Auto Scaling itself answers a describe for.
        let group_name = nodegroup["resources"]["autoScalingGroups"][0]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !group_name.is_empty(),
            "case {case:?}: expected the group to report its Auto Scaling group, got {read}"
        );
        let (status, described) = autoscaling(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group_name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeAutoScalingGroups, got {status} \
             with body {described}"
        );
        assert!(
            xml_texts(&described, "AutoScalingGroupName").contains(&group_name),
            "case {case:?}: expected Auto Scaling to hold {group_name:?}, got {described}"
        );

        // And the attribute the stack publishes is the ARN Amazon EKS minted.
        assert_eq!(
            output_value(&body, "NodegroupArn").map(Value::from),
            Some(nodegroup["nodegroupArn"].clone()),
            "case {case:?}: expected the stack to report the group's own ARN, got {body}"
        );
    }
}

/// The same two types declared TOGETHER with the network and the cluster they depend on: one
/// template, the add-on and the node group naming the cluster through `Ref`, so each is
/// created only once the resource it depends on is one Amazon EKS reports ready — and the
/// subnets the nodes launch into are the ones this stack made in EC2.
#[tokio::test]
async fn one_template_provisions_the_cluster_and_the_workloads_declared_on_it() {
    let router = router();
    let case = "the network, the cluster, an add-on and a node group in one template";
    let stack_name = "eks-cluster-estate";
    let mut resources = network_resources(2);
    resources.insert(
        CLUSTER_ID.to_string(),
        json!({
            "Type": "AWS::EKS::Cluster",
            "Properties": {
                "RoleArn": role_arn("eksClusterRole"),
                "ResourcesVpcConfig": { "SubnetIds": subnet_refs(2) }
            }
        }),
    );
    resources.insert(
        ADDON_ID.to_string(),
        json!({
            "Type": ADDON,
            "Properties": {
                "ClusterName": { "Ref": CLUSTER_ID },
                "AddonName": "vpc-cni",
                "ResolveConflicts": "OVERWRITE"
            }
        }),
    );
    resources.insert(
        NODEGROUP_ID.to_string(),
        json!({
            "Type": NODEGROUP,
            "Properties": {
                "ClusterName": { "Ref": CLUSTER_ID },
                "NodegroupName": "estate-nodes",
                "NodeRole": role_arn("eksNodeRole"),
                "Subnets": subnet_refs(2),
                "ScalingConfig": { "MinSize": 1, "DesiredSize": 1, "MaxSize": 2 }
            }
        }),
    );
    let template = json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            "ClusterName": { "Value": { "Ref": CLUSTER_ID } },
            "AddonArn": { "Value": { "Fn::GetAtt": [ADDON_ID, "Arn"] } },
            "NodegroupArn": { "Value": { "Fn::GetAtt": [NODEGROUP_ID, "Arn"] } }
        }
    })
    .to_string();

    let body = deployed(&router, case, stack_name, &template).await;
    let cluster_name = output_value(&body, "ClusterName")
        .unwrap_or_else(|| panic!("case {case:?}: no ClusterName output in {body}"));

    let (status, read) = describe_addon(&router, &cluster_name, "vpc-cni").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the declared add-on to be readable, got {status} with body \
         {read}"
    );
    assert_eq!(
        read["addon"]["status"],
        json!(ACTIVE),
        "case {case:?}: expected the add-on {ACTIVE}, got {read}"
    );
    assert_eq!(
        output_value(&body, "AddonArn").map(Value::from),
        Some(read["addon"]["addonArn"].clone()),
        "case {case:?}: expected the stack to report the add-on's own ARN, got {body}"
    );

    let (status, read) = describe_nodegroup(&router, &cluster_name, "estate-nodes").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the declared node group to be readable, got {status} with \
         body {read}"
    );
    let nodegroup = &read["nodegroup"];
    assert_eq!(
        nodegroup["status"],
        json!(ACTIVE),
        "case {case:?}: expected the node group {ACTIVE}, got {read}"
    );
    assert_eq!(
        output_value(&body, "NodegroupArn").map(Value::from),
        Some(nodegroup["nodegroupArn"].clone()),
        "case {case:?}: expected the stack to report the group's own ARN, got {body}"
    );
    for subnet in strings_of(&nodegroup["subnets"]) {
        let (status, described) = ec2(&router, "DescribeSubnets", &[("SubnetId.1", &subnet)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {subnet:?} to exist in EC2, got {status} with body \
             {described}"
        );
    }
}

/// The rule's refusal half: a declaration Amazon EKS (or this emulator) cannot satisfy FAILS
/// the resource and the stack rather than completing it with nothing created — which is the
/// whole point of holding the resource's creation open on that service's own answer.
///
/// The rows differ in what cannot be satisfied: an add-on no catalog publishes, a node group
/// whose subnets name nothing EC2 holds, a node group at a Kubernetes version that is not its
/// cluster's, and a property no arm maps onto the owning API's create.
#[tokio::test]
async fn a_stack_fails_when_the_declared_workload_cannot_be_created() {
    let router = router();
    let (cluster_name, subnets) = deploy_host(&router, "eks-workload-refused-host", 2).await;
    let node_role = role_arn("eksNodeRole");
    let logical_id = "Workload";
    // (case, the resource type, the properties declared for it, the collection and the name
    // nothing may exist under afterwards)
    let cases: [(&str, &str, Value, (&str, &str)); 4] = [
        (
            "an add-on name the catalog publishes no build of",
            ADDON,
            json!({
                "ClusterName": cluster_name,
                "AddonName": "not-an-addon-this-catalog-publishes"
            }),
            ("addons", "not-an-addon-this-catalog-publishes"),
        ),
        (
            "a node group whose subnets name nothing EC2 holds",
            NODEGROUP,
            json!({
                "ClusterName": cluster_name,
                "NodegroupName": "refused-subnets",
                "NodeRole": node_role,
                "Subnets": ["subnet-0123456789abcdef0"]
            }),
            ("node-groups", "refused-subnets"),
        ),
        (
            "a node group at a Kubernetes version that is not its cluster's",
            NODEGROUP,
            json!({
                "ClusterName": cluster_name,
                "NodegroupName": "refused-version",
                "NodeRole": node_role,
                "Subnets": [subnets[0], subnets[1]],
                // A version no cluster of this emulator runs, so the create cannot match it
                // to the cluster's own.
                "Version": "1.21"
            }),
            ("node-groups", "refused-version"),
        ),
        (
            "a node group property no arm maps onto CreateNodegroup",
            NODEGROUP,
            json!({
                "ClusterName": cluster_name,
                "NodegroupName": "refused-unmapped",
                "NodeRole": node_role,
                "Subnets": [subnets[0], subnets[1]],
                "ForceUpdateEnabled": true
            }),
            ("node-groups", "refused-unmapped"),
        ),
    ];

    for (index, (case, resource_type, properties, (path, name))) in cases.into_iter().enumerate() {
        let stack_name = format!("eks-workload-refused-{index}");
        let template = json!({
            "Resources": { logical_id: { "Type": resource_type, "Properties": properties } }
        })
        .to_string();
        let (status, created) = create_stack(&router, &stack_name, &template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — the type is one a template \
             may declare — got {status} with body {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, &stack_name).await;
        assert_eq!(
            reached, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to fail rather than complete, got {reached} \
             with failure reasons {:?}",
            stack_event_reasons(&router, REGION, &stack_name).await
        );
        let body = settled_stack(case, &router, REGION, &stack_name).await;
        assert!(
            xml_text(&body, "StackStatusReason")
                .is_some_and(|reason| reason.contains(logical_id)),
            "case {case:?}: expected the failure to name the declared resource, got {body}"
        );

        // And nothing was left behind for a resource the stack does not record.
        let (status, read) = eks(
            &router,
            "GET",
            &format!("/clusters/{cluster_name}/{path}/{name}"),
            None,
        )
        .await;
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected no {path} resource for a failed declaration, got {status} \
             with body {read}"
        );
    }
}
