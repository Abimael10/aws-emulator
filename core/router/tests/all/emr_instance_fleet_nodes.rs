// SPDX-License-Identifier: CC0-1.0

//! The Amazon EC2 instances an INSTANCE-FLEET cluster was provisioned from, read the way a
//! client walks a cluster down to its nodes: `ListInstances` narrowed to one fleet role, and
//! then Amazon EC2's own `DescribeInstances` on each `Ec2InstanceId` that listing names.
//!
//! THE RULE under test: `ListInstances` "provides information for all active Amazon EC2
//! instances" of a cluster
//! (`smithy_query(query="operation",operation="ListInstances",model_path="services/service-com-amazonaws-emr-elasticmapreduce/smithy-model/com-amazonaws-emr-elasticmapreduce.json")`),
//! so it reports one record per instance the cluster's collections were provisioned from and
//! `InstanceFleetType` ("the node type of the instance fleet") narrows that listing to the
//! fleet filling that role — each record naming that fleet, the instance type the fleet
//! provisions, the purchasing option it provisions under and an `Ec2InstanceId` Amazon EC2
//! itself holds, which Amazon EC2 reports in the subnet the launch placed the cluster in and
//! tagged with the cluster's id, the role its collection fills and the cluster's own
//! propagated tags. A role no fleet of the cluster fills selects nothing.
//!
//! Only a cross-service suite can pin this: IAM holds the service role and the instance
//! profile a launch names, Amazon EC2 owns the subnet and the instances, and Amazon EMR asks
//! Amazon EC2 for the capacity — so every id asserted here is one the service that owns it
//! minted, and the suite composes none of them. It therefore runs against the whole
//! `build_router` service set, which is this suite's increment over the owning crate's own
//! `tests/all/cluster_instances.rs`: that one mounts Amazon EMR beside Amazon EC2 and already
//! covers the listing's filter table and its `Marker` paging, so neither is repeated here.
//!
//! The rows span what the rule generalises over: the role of the fleet asked for (`MASTER`,
//! `CORE`, `TASK`, and one no fleet of the cluster fills), the instance type a fleet
//! provisions, how a fleet states its capacity (one unit per instance, or weighted units that
//! need two instances), and how many fleets the cluster carries.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The zone the subnet every cluster here is placed in lives in.
const ZONE: &str = "us-west-2a";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";

/// The roles every launch here names: the service role Amazon EMR assumes, and the instance
/// profile its Amazon EC2 instances come up with.
const SERVICE_ROLE: &str = "EMR_DefaultRole";
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";

/// The release every launch here names.
const RELEASE_LABEL: &str = "emr-7.9.0";

/// A tag of the cluster's own, which "a list of tags to associate with a cluster and
/// propagate to Amazon EC2 instances" (`RunJobFlow$Tags`) puts on every node it provisions.
const CLUSTER_TAG_KEY: &str = "team";
const CLUSTER_TAG_VALUE: &str = "analytics";

/// The service-managed tags Amazon EMR puts on an Amazon EC2 instance it creates for a
/// cluster (Amazon EMR Management Guide, "Tagging Amazon EMR clusters"), spelled out here
/// rather than imported from the service crate so a wrong constant in the implementation
/// cannot make a case below pass.
const CLUSTER_ID_TAG: &str = "aws:elasticmapreduce:job-flow-id";
const INSTANCE_ROLE_TAG: &str = "aws:elasticmapreduce:instance-group-role";

/// Every role an instance fleet may fill (`com.amazonaws.emr#InstanceFleetType`), which is
/// what each cluster below is asked for one role at a time.
const FLEET_ROLES: [&str; 3] = ["MASTER", "CORE", "TASK"];

/// How long a look for a clock-owed move keeps looking before giving up, and how often it
/// looks. Generous and only here so a failure is a failure rather than a hang: every case
/// waits for the CONDITION — it reads again until the listing holds — so a loaded host
/// delays a case instead of breaking it.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Send one request to the service `signing_name` addresses, and answer `(status, body)`.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: Option<&str>,
    content_type: &str,
    body: String,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(
        router,
        "ec2",
        None,
        "application/x-www-form-urlencoded",
        form(action, EC2_API_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One IAM action that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) {
    let (status, body) = send(
        router,
        "iam",
        None,
        "application/x-www-form-urlencoded",
        form(action, IAM_API_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with body {body}"
    );
}

/// One Amazon EMR operation (`awsJson1_1`, `ElasticMapReduce.<Operation>`) that must be
/// served.
async fn emr(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = send(
        router,
        "elasticmapreduce",
        Some(&format!("ElasticMapReduce.{operation}")),
        "application/x-amz-json-1.1",
        body.to_string(),
    )
    .await;
    let parsed: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// The subnet of one VPC Amazon EC2 itself minted, plus the service role and the instance
/// profile a launch names — each created through the service that owns it.
async fn seed(router: &MultiServiceRouter) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc.as_str()),
                p("CidrBlock", "10.90.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    );
    for (role, principal) in [
        (SERVICE_ROLE, "elasticmapreduce.amazonaws.com"),
        (INSTANCE_PROFILE, "ec2.amazonaws.com"),
    ] {
        iam(
            router,
            "CreateRole",
            &[
                p("RoleName", role),
                p("AssumeRolePolicyDocument", trust_policy(principal)),
            ],
        )
        .await;
    }
    iam(
        router,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", INSTANCE_PROFILE), p("Path", "/")],
    )
    .await;
    iam(
        router,
        "AddRoleToInstanceProfile",
        &[
            p("InstanceProfileName", INSTANCE_PROFILE),
            p("RoleName", INSTANCE_PROFILE),
        ],
    )
    .await;
    subnet
}

/// A trust policy admitting `principal` and nothing else.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Launch a kept-alive cluster whose capacity is stated as `fleets`, placed in `subnet`, and
/// answer the cluster id Amazon EMR minted for it.
async fn launch(router: &MultiServiceRouter, case: &str, subnet: &str, fleets: Value) -> String {
    let request = json!({
        "Name": case,
        "ReleaseLabel": RELEASE_LABEL,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Tags": [{ "Key": CLUSTER_TAG_KEY, "Value": CLUSTER_TAG_VALUE }],
        "Instances": {
            "InstanceFleets": fleets,
            "Ec2SubnetIds": [subnet],
            "KeepJobFlowAliveWhenNoSteps": true,
        },
    });
    let answered = emr(router, case, "RunJobFlow", &request).await;
    answered["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: RunJobFlow must answer a JobFlowId, got {answered}"))
        .to_string()
}

/// One instance fleet config: a role, a target of On-Demand units, and the one instance type
/// it may provision them from — with `weight` units per instance where it states one.
fn fleet(role: &str, instance_type: &str, units: i64, weight: Option<i64>) -> Value {
    let mut config = json!({ "InstanceType": instance_type });
    if let Some(weight) = weight {
        config["WeightedCapacity"] = json!(weight);
    }
    json!({
        "Name": format!("{role}-fleet"),
        "InstanceFleetType": role,
        "TargetOnDemandCapacity": units,
        "InstanceTypeConfigs": [config],
    })
}

/// The fleets of `cluster_id`, as the role each fills paired with the id Amazon EMR minted
/// for it — read through `ListInstanceFleets`, so a listing's `InstanceFleetId` is compared
/// against the fleet the cluster itself reports.
async fn fleets_by_role(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
) -> BTreeMap<String, String> {
    let answered = emr(
        router,
        case,
        "ListInstanceFleets",
        &json!({ "ClusterId": cluster_id }),
    )
    .await;
    answered["InstanceFleets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: ListInstanceFleets must answer fleets, got {answered}"))
        .iter()
        .filter_map(|held| {
            Some((
                held["InstanceFleetType"].as_str()?.to_string(),
                held["Id"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// The instances `ListInstances` reports for `cluster_id` under `filters`.
///
/// Every cluster here holds far fewer instances than one page reports, so the answer must
/// carry no `Marker`: the whole listing is what this reads.
async fn listed_instances(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    filters: &Value,
) -> Vec<Value> {
    let mut request = filters.clone();
    request["ClusterId"] = json!(cluster_id);
    let answered = emr(router, case, "ListInstances", &request).await;
    assert!(
        answered["Marker"].is_null(),
        "case {case:?}: a listing that fits one page carries no Marker, got {answered}"
    );
    answered["Instances"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: ListInstances must answer Instances, got {answered}"))
        .clone()
}

/// The instances `ListInstances` reports under `filters` once there are `expected` of them
/// and every one has reached `RUNNING` — looked for until the deadline passes rather than
/// waited out, since the move out of `PROVISIONING` is one the clock owes.
async fn settled_instances(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    filters: &Value,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let listed = listed_instances(router, case, cluster_id, filters).await;
        let settled = listed
            .iter()
            .all(|instance| instance["Status"]["State"].as_str() == Some("RUNNING"));
        if listed.len() == expected && settled {
            return listed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} RUNNING instances under {filters}, got {listed:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One instance as Amazon EC2 reports it.
#[derive(Debug)]
struct Node {
    instance_type: Option<String>,
    subnet: Option<String>,
    tags: BTreeMap<String, String>,
}

/// The instance Amazon EC2 holds under `ec2_instance_id`, read through `DescribeInstances`
/// naming exactly that instance — which is where a client resolves what a cluster named.
///
/// The read names exactly ONE instance id, so the answer carries that instance's facts and
/// nothing else to confuse them with; each tag is still paired inside its own `<item>`, so a
/// tag never takes another's value.
async fn described(router: &MultiServiceRouter, case: &str, ec2_instance_id: &str) -> Node {
    let body = ec2(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", ec2_instance_id)],
    )
    .await;
    assert!(
        body.contains(ec2_instance_id),
        "case {case:?}: Amazon EC2 must hold {ec2_instance_id}, which the cluster reports as \
         one of its instances, got {body}"
    );
    Node {
        instance_type: elements(&body, "instanceType").into_iter().next(),
        subnet: elements(&body, "subnetId").into_iter().next(),
        tags: items_of(&body)
            .into_iter()
            .filter_map(|item| {
                let key = elements(item, "key").into_iter().next()?;
                let value = elements(item, "value").into_iter().next()?;
                Some((key, value))
            })
            .collect(),
    }
}

/// One cluster shape: the fleets its launch states, and one entry per instance those fleets
/// provision — the role of the fleet it belongs to, and the type it is launched as.
struct Row {
    case: &'static str,
    fleets: Value,
    nodes: Vec<(&'static str, &'static str)>,
}

/// THE RULE: every fleet of a cluster is reported down to the Amazon EC2 instances it was
/// provisioned from, one `ListInstances` record each, and each record resolves in Amazon EC2
/// to an instance of that type in the cluster's subnet carrying the cluster's id, its role
/// and the cluster's own tags. A role no fleet of the cluster fills reports nothing.
#[tokio::test]
async fn a_fleet_listing_joins_each_fleet_to_the_ec2_instances_it_provisioned() {
    let rows = vec![
        Row {
            case: "a core fleet of one on-demand unit beside its primary",
            fleets: json!([
                fleet("MASTER", "m5.xlarge", 1, None),
                fleet("CORE", "m5.2xlarge", 1, None),
            ]),
            nodes: vec![("MASTER", "m5.xlarge"), ("CORE", "m5.2xlarge")],
        },
        Row {
            case: "a core fleet whose weighted units need two instances",
            fleets: json!([
                fleet("MASTER", "m5.xlarge", 1, None),
                fleet("CORE", "m5.4xlarge", 4, Some(2)),
            ]),
            nodes: vec![
                ("MASTER", "m5.xlarge"),
                ("CORE", "m5.4xlarge"),
                ("CORE", "m5.4xlarge"),
            ],
        },
        Row {
            case: "a cluster whose task capacity is a fleet of its own",
            fleets: json!([
                fleet("MASTER", "m5.xlarge", 1, None),
                fleet("CORE", "m5.2xlarge", 1, None),
                fleet("TASK", "m5.large", 2, None),
            ]),
            nodes: vec![
                ("MASTER", "m5.xlarge"),
                ("CORE", "m5.2xlarge"),
                ("TASK", "m5.large"),
                ("TASK", "m5.large"),
            ],
        },
    ];

    let router = router();
    let subnet = seed(&router).await;
    for row in &rows {
        let case = row.case;
        let cluster_id = launch(&router, case, &subnet, row.fleets.clone()).await;
        let fleets = fleets_by_role(&router, case, &cluster_id).await;

        for role in FLEET_ROLES {
            let wanted: Vec<&str> = row
                .nodes
                .iter()
                .filter(|(filled, _)| *filled == role)
                .map(|(_, instance_type)| *instance_type)
                .collect();
            let filters = json!({ "InstanceFleetType": role });
            let listed = if wanted.is_empty() {
                // Nothing is owed here, so the listing is read ONCE: a role no fleet of the
                // cluster fills must select nothing straight away.
                listed_instances(&router, case, &cluster_id, &filters).await
            } else {
                settled_instances(&router, case, &cluster_id, &filters, wanted.len()).await
            };

            let mut reported: Vec<String> = listed
                .iter()
                .map(|instance| instance["InstanceType"].as_str().unwrap_or_default().to_string())
                .collect();
            reported.sort();
            let mut expected: Vec<String> =
                wanted.iter().map(|kind| (*kind).to_string()).collect();
            expected.sort();
            assert_eq!(
                reported, expected,
                "case {case:?}: the {role} fleet must report the instances it provisions, \
                 expected {expected:?}, got {listed:?}"
            );

            let Some(fleet_id) = fleets.get(role) else {
                continue;
            };
            for instance in &listed {
                assert_eq!(
                    instance["InstanceFleetId"].as_str(),
                    Some(fleet_id.as_str()),
                    "case {case:?}: an instance of the {role} fleet names it, got {instance}"
                );
                assert!(
                    instance["InstanceGroupId"].is_null(),
                    "case {case:?}: an instance of a fleet names no instance group, got \
                     {instance}"
                );
                assert_eq!(
                    instance["Market"].as_str(),
                    Some("ON_DEMAND"),
                    "case {case:?}: an instance reports the option its fleet provisions it \
                     under, got {instance}"
                );
                let ec2_instance_id = instance["Ec2InstanceId"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: an instance names the EC2 instance it is, got {instance}")
                });
                let node = described(&router, case, ec2_instance_id).await;
                assert_eq!(
                    node.instance_type.as_deref(),
                    instance["InstanceType"].as_str(),
                    "case {case:?}: {ec2_instance_id} must be of the type the cluster reports, \
                     got {node:?}"
                );
                assert_eq!(
                    node.subnet.as_deref(),
                    Some(subnet.as_str()),
                    "case {case:?}: {ec2_instance_id} must run in the cluster's subnet, got \
                     {node:?}"
                );
                assert_eq!(
                    node.tags.get(CLUSTER_ID_TAG).map(String::as_str),
                    Some(cluster_id.as_str()),
                    "case {case:?}: {ec2_instance_id} must carry its cluster's id, got {node:?}"
                );
                assert_eq!(
                    node.tags.get(INSTANCE_ROLE_TAG).map(String::as_str),
                    Some(role),
                    "case {case:?}: {ec2_instance_id} must carry the role its fleet fills, got \
                     {node:?}"
                );
                assert_eq!(
                    node.tags.get(CLUSTER_TAG_KEY).map(String::as_str),
                    Some(CLUSTER_TAG_VALUE),
                    "case {case:?}: {ec2_instance_id} must carry the cluster's own tag, got \
                     {node:?}"
                );
            }
        }
    }
}
