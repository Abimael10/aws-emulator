// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::MemoryDB::ParameterGroup` or an
//! `AWS::MemoryDB::SubnetGroup` really creates them in Amazon MemoryDB.
//!
//! The rule under test: **a stack reaches a successful terminal state only once it has
//! created every declared resource of a type this emulator provisions through the OWNING
//! service's own create API, so each one is readable through that service's own read right
//! after the stack completes — carrying the configuration the template declared, and the
//! same ARN the stack publishes for it — and deleting the stack deletes it there.** Three
//! crates have to agree for that to hold (CloudFormation makes the calls; Amazon MemoryDB
//! serves them, and Amazon EC2 answers for the subnets a subnet group places a cluster in),
//! so the suite lives here rather than in any one of them.
//!
//! What a client does with such a stack is the shape of every case: it reads `Outputs` into
//! a map (`{o["OutputKey"]: o["OutputValue"]}`, which an output carrying no value raises
//! `KeyError` on) and then drives MemoryDB's own API by the names and ARNs it found. A
//! `CREATE_COMPLETE` stack whose parameter group `DescribeParameterGroups` answers
//! `ParameterGroupNotFoundFault` for is not a state AWS can be in.
//!
//! The rows differ along what the rule generalises over: which of the two group types the
//! template declares (each alone, and both together with the network a subnet group needs),
//! whether the template NAMES each group or leaves the name to CloudFormation, whether the
//! declaration carries the optional `Description`, `Tags` and (for a parameter group) the
//! `Parameters` values, how many subnets the subnet group lists, which spelling the template
//! is written in (JSON and YAML), and which Region the stack is deployed in.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// How long a case waits for a stack operation to reach a terminal status before giving up:
/// the stack owes each of its resources a clock-driven span, so the status is not terminal
/// on the first read. Generous on purpose — the case polls for the CONDITION and stops as
/// soon as it holds, so this bounds only a stack that never settles.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The engine family every parameter group below is created with, one the MemoryDB engine
/// catalogue publishes (`DescribeEngineVersions$ParameterGroupFamily`).
const FAMILY: &str = "memorydb_valkey7";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_memorydb_amazonmemorydb::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon MemoryDB operation over its awsJson1_1 wire, signed for `region`.
async fn memorydb(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("memory-db.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonMemoryDB.{operation}"))
        .header("authorization", authorization("memorydb", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The wire error code of a MemoryDB failure body (`__type`, with any namespace prefix
/// dropped).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Read the stack until it reports a status that is no longer in progress, and answer that
/// read's body. The condition is polled for, so a case takes only as long as the stack does.
async fn poll_to_terminal(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack_name: &str,
) -> String {
    let started = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} {described}"
        );
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        if !stack_status.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the stack stayed {stack_status:?} rather than settling"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every output of the described stack as the key/value pairs a client reads it as, with the
/// read the issue filed asserted first: an output reported with a key and no value is
/// missing from this map.
fn outputs(case: &str, described: &str) -> BTreeMap<String, String> {
    let read = outputs_of(described);
    for key in [
        "ParameterGroupRef",
        "ParameterGroupArn",
        "SubnetGroupRef",
        "SubnetGroupArn",
        "VpcId",
        "SubnetId",
    ] {
        if described.contains(&format!("<OutputKey>{key}</OutputKey>")) {
            assert!(
                read.contains_key(key),
                "case {case:?}: every Output of a complete stack carries an OutputValue; \
                 {key:?} carries none ({described})"
            );
        }
    }
    read
}

/// The template every row deploys: the network a subnet group is placed in, the parameter
/// group and the subnet group, with every return value read back as an output.
///
/// `named` decides whether the template states each group's name or leaves it to
/// CloudFormation.
fn template(named: bool, zone: &str) -> String {
    let parameter_group_name = if named {
        r#""ParameterGroupName": "probe-parameters","#
    } else {
        ""
    };
    let subnet_group_name = if named {
        r#""SubnetGroupName": "probe-subnets","#
    } else {
        ""
    };
    json_body(parameter_group_name, subnet_group_name, zone)
}

/// The JSON spelling of that template.
fn json_body(parameter_group_name: &str, subnet_group_name: &str, zone: &str) -> String {
    format!(
        r#"{{"AWSTemplateFormatVersion": "2010-09-09",
             "Resources": {{
               "Vpc": {{"Type": "AWS::EC2::VPC",
                 "Properties": {{"CidrBlock": "10.40.0.0/16"}}}},
               "Subnet": {{"Type": "AWS::EC2::Subnet",
                 "Properties": {{"VpcId": {{"Ref": "Vpc"}}, "CidrBlock": "10.40.1.0/24",
                   "AvailabilityZone": "{zone}"}}}},
               "ParameterGroup": {{"Type": "AWS::MemoryDB::ParameterGroup",
                 "Properties": {{{parameter_group_name}
                   "Family": "{FAMILY}",
                   "Description": "the engine settings of a probe estate",
                   "Parameters": {{"maxmemory-policy": "allkeys-lru"}},
                   "Tags": [{{"Key": "tier", "Value": "cache"}}]}}}},
               "SubnetGroup": {{"Type": "AWS::MemoryDB::SubnetGroup",
                 "Properties": {{{subnet_group_name}
                   "Description": "the placement of a probe estate",
                   "SubnetIds": [{{"Ref": "Subnet"}}],
                   "Tags": [{{"Key": "tier", "Value": "cache"}}]}}}}}},
             "Outputs": {{
               "VpcId": {{"Value": {{"Ref": "Vpc"}}}},
               "SubnetId": {{"Value": {{"Ref": "Subnet"}}}},
               "ParameterGroupRef": {{"Value": {{"Ref": "ParameterGroup"}}}},
               "ParameterGroupArn": {{"Value": {{"Fn::GetAtt": ["ParameterGroup", "ARN"]}}}},
               "SubnetGroupRef": {{"Value": {{"Ref": "SubnetGroup"}}}},
               "SubnetGroupArn": {{"Value": {{"Fn::GetAtt": ["SubnetGroup", "ARN"]}}}}}}}}"#
    )
}

/// The YAML spelling of the same estate, with the short-form intrinsics a client writes and
/// TWO subnets in the group.
fn yaml_body(zone: &str, other_zone: &str) -> String {
    format!(
        "\
AWSTemplateFormatVersion: '2010-09-09'
Resources:
  Vpc:
    Type: AWS::EC2::VPC
    Properties:
      CidrBlock: 10.41.0.0/16
  Subnet:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.41.1.0/24
      AvailabilityZone: {zone}
  OtherSubnet:
    Type: AWS::EC2::Subnet
    Properties:
      VpcId: !Ref Vpc
      CidrBlock: 10.41.2.0/24
      AvailabilityZone: {other_zone}
  ParameterGroup:
    Type: AWS::MemoryDB::ParameterGroup
    Properties:
      ParameterGroupName: yaml-parameters
      Family: {FAMILY}
  SubnetGroup:
    Type: AWS::MemoryDB::SubnetGroup
    Properties:
      SubnetGroupName: yaml-subnets
      SubnetIds:
        - !Ref Subnet
        - !Ref OtherSubnet
Outputs:
  VpcId:
    Value: !Ref Vpc
  SubnetId:
    Value: !Ref Subnet
  ParameterGroupRef:
    Value: !Ref ParameterGroup
  ParameterGroupArn:
    Value: !GetAtt ParameterGroup.ARN
  SubnetGroupRef:
    Value: !Ref SubnetGroup
  SubnetGroupArn:
    Value: !GetAtt SubnetGroup.ARN
"
    )
}

/// One row of the table: a template, and what the rule says it must leave in Amazon
/// MemoryDB.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the groups are created in and the
    /// one their ARNs carry.
    region: &'static str,
    stack_name: &'static str,
    /// The template body, in the form the row writes it.
    template: fn() -> String,
    /// The name the template states for each group, or `None` where it states none and
    /// CloudFormation generates it.
    parameter_group_name: Option<&'static str>,
    subnet_group_name: Option<&'static str>,
    /// How many subnets the subnet group lists.
    subnets: usize,
    /// The parameter value the declaration states for the parameter group, if any.
    parameter: Option<(&'static str, &'static str)>,
    /// The tags the declaration carries, which `ListTags` must then report for the group.
    tags: &'static [(&'static str, &'static str)],
}

fn rows() -> Vec<Row> {
    vec![
        // The reported estate: a named parameter group and a named subnet group in a VPC of
        // the same stack, each read back through MemoryDB by the name and the ARN the stack
        // published.
        Row {
            case: "named groups with a description, parameters and tags, in JSON",
            region: "us-east-1",
            stack_name: "memorydb-estate",
            template: || template(true, "us-east-1a"),
            parameter_group_name: Some("probe-parameters"),
            subnet_group_name: Some("probe-subnets"),
            subnets: 1,
            parameter: Some(("maxmemory-policy", "allkeys-lru")),
            tags: &[("tier", "cache")],
        },
        // The same estate with both names left to CloudFormation, in another Region: the
        // groups exist under the names it generated, and the ARNs carry that Region.
        Row {
            case: "groups whose names CloudFormation generates, in another Region",
            region: "eu-west-1",
            stack_name: "memorydb-generated",
            template: || template(false, "eu-west-1a"),
            parameter_group_name: None,
            subnet_group_name: None,
            subnets: 1,
            parameter: Some(("maxmemory-policy", "allkeys-lru")),
            tags: &[("tier", "cache")],
        },
        // The YAML spelling, with no description, no tags, no declared parameters, and TWO
        // subnets of the same VPC in the group.
        Row {
            case: "the YAML spelling, two subnets and no optional members",
            region: "us-east-1",
            stack_name: "memorydb-yaml",
            template: || yaml_body("us-east-1a", "us-east-1b"),
            parameter_group_name: Some("yaml-parameters"),
            subnet_group_name: Some("yaml-subnets"),
            subnets: 2,
            parameter: None,
            tags: &[],
        },
    ]
}

/// Every row: the stack reaches `CREATE_COMPLETE`, every declared output carries a value,
/// and the very next read through Amazon MemoryDB's own API resolves both groups by the
/// names and ARNs the stack published — with the configuration the template declared.
#[tokio::test]
async fn a_stack_declaring_memorydb_groups_creates_them_in_memorydb() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = (row.template)();
        let (status, answered) = cloudformation(
            &router,
            row.region,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {answered}"
        );
        assert!(
            answered.contains(":stack/"),
            "case {case:?}: CreateStack must answer the new stack's id, got {answered}"
        );
        // The stack's own ID, which is how a DELETED stack is read: "Deleted stacks: You
        // must specify the unique stack ID" (`DescribeStacksInput$StackName`).
        let stack_id = opt_element(&answered, "StackId").unwrap_or_default();
        let described = poll_to_terminal(&router, row.region, case, row.stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );
        let out = outputs(case, &described);

        // The names the stack published are the ones the template stated, where it stated
        // them; a generated name is CloudFormation's own, and what the rule says about it is
        // that the group MemoryDB holds under it is this stack's.
        for (declared, published) in [
            (row.parameter_group_name, "ParameterGroupRef"),
            (row.subnet_group_name, "SubnetGroupRef"),
        ] {
            let read = out.get(published).map(String::as_str).unwrap_or_default();
            if let Some(declared) = declared {
                assert_eq!(
                    read, declared,
                    "case {case:?}: expected {published} to be the declared name, got {read:?}"
                );
            } else {
                assert!(
                    !read.is_empty(),
                    "case {case:?}: expected {published} to carry the generated name, got \
                     {read:?}"
                );
            }
        }

        // THE PARAMETER GROUP, read through the service that owns it, by the name the stack
        // published.
        let parameter_group_name = out["ParameterGroupRef"].clone();
        let (status, answered) = memorydb(
            &router,
            row.region,
            "DescribeParameterGroups",
            &json!({ "ParameterGroupName": parameter_group_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a CREATE_COMPLETE stack's parameter group must be readable in \
             MemoryDB, got {status} with {answered}"
        );
        let group = &answered["ParameterGroups"][0];
        assert_eq!(
            group["ARN"],
            json!(out["ParameterGroupArn"]),
            "case {case:?}: expected the stack's ParameterGroupArn to be the ARN MemoryDB \
             reports, got {group}"
        );
        assert_eq!(
            group["ARN"],
            json!(format!(
                "arn:aws:memorydb:{}:{ACCOUNT_ID}:parametergroup/{}",
                row.region,
                parameter_group_name.to_ascii_lowercase()
            )),
            "case {case:?}: expected the ARN that service's own naming gives the group, got \
             {group}"
        );
        assert_eq!(
            group["Family"],
            json!(FAMILY),
            "case {case:?}: expected the declared family, got {group}"
        );

        // The parameter values the declaration stated are the ones that service reports for
        // the group.
        if let Some((parameter, value)) = row.parameter {
            let (status, answered) = memorydb(
                &router,
                row.region,
                "DescribeParameters",
                &json!({ "ParameterGroupName": parameter_group_name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeParameters must answer 200, got {status} with \
                 {answered}"
            );
            let reported = answered["Parameters"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|entry| entry["Name"] == json!(parameter))
                .unwrap_or(Value::Null);
            assert_eq!(
                reported["Value"],
                json!(value),
                "case {case:?}: expected the declared value of {parameter:?}, got {reported}"
            );
        }

        // THE SUBNET GROUP, read the same way: the subnets the template declared, in the VPC
        // the stack created them in — the VPC Amazon EC2 reports for them, which is the one
        // the stack's own output names.
        let subnet_group_name = out["SubnetGroupRef"].clone();
        let (status, answered) = memorydb(
            &router,
            row.region,
            "DescribeSubnetGroups",
            &json!({ "SubnetGroupName": subnet_group_name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a CREATE_COMPLETE stack's subnet group must be readable in \
             MemoryDB, got {status} with {answered}"
        );
        let group = &answered["SubnetGroups"][0];
        assert_eq!(
            group["ARN"],
            json!(out["SubnetGroupArn"]),
            "case {case:?}: expected the stack's SubnetGroupArn to be the ARN MemoryDB \
             reports, got {group}"
        );
        assert_eq!(
            group["ARN"],
            json!(format!(
                "arn:aws:memorydb:{}:{ACCOUNT_ID}:subnetgroup/{}",
                row.region,
                subnet_group_name.to_ascii_lowercase()
            )),
            "case {case:?}: expected the ARN that service's own naming gives the group, got \
             {group}"
        );
        assert_eq!(
            group["VpcId"],
            json!(out["VpcId"]),
            "case {case:?}: expected the subnet group to report the VPC its subnets are in, \
             got {group}"
        );
        let subnets = group["Subnets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            subnets.len(),
            row.subnets,
            "case {case:?}: expected {} subnet(s) in the group, got {group}",
            row.subnets
        );
        assert!(
            subnets
                .iter()
                .any(|subnet| subnet["Identifier"] == json!(out["SubnetId"])),
            "case {case:?}: expected the subnet the stack created to be in the group, got \
             {group}"
        );
        assert!(
            subnets
                .iter()
                .all(|subnet| subnet["AvailabilityZone"]["Name"]
                    .as_str()
                    .is_some_and(|zone| zone.starts_with(row.region))),
            "case {case:?}: expected each subnet's zone to be one of the stack's Region, got \
             {group}"
        );

        // The tags the declaration asked for are the ones each group's ARN answers.
        for arn in [&out["ParameterGroupArn"], &out["SubnetGroupArn"]] {
            let (status, answered) = memorydb(
                &router,
                row.region,
                "ListTags",
                &json!({ "ResourceArn": arn }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListTags on {arn} must answer 200, got {status} with \
                 {answered}"
            );
            let reported: BTreeMap<String, String> = answered["TagList"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|tag| {
                    Some((
                        tag["Key"].as_str()?.to_string(),
                        tag["Value"].as_str()?.to_string(),
                    ))
                })
                .collect();
            let expected: BTreeMap<String, String> = row
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the declared tags on {arn}, got {answered}"
            );
        }

        // DELETING THE STACK deletes both groups there: a group a stack no longer records
        // must not be left behind in the service that holds it.
        let (status, answered) = cloudformation(
            &router,
            row.region,
            "DeleteStack",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must answer 200, got {status} {answered}"
        );
        let described = poll_to_terminal(&router, row.region, case, &stack_id).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("DELETE_COMPLETE"),
            "case {case:?}: expected the delete to complete, got {described}"
        );
        for (operation, member, name, fault) in [
            (
                "DescribeParameterGroups",
                "ParameterGroupName",
                &parameter_group_name,
                "ParameterGroupNotFoundFault",
            ),
            (
                "DescribeSubnetGroups",
                "SubnetGroupName",
                &subnet_group_name,
                "SubnetGroupNotFoundFault",
            ),
        ] {
            let (status, answered) =
                memorydb(&router, row.region, operation, &json!({ member: name })).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, fault),
                "case {case:?}: expected {operation} to answer the modelled {fault} for a \
                 group the stack deleted, got {status} with {answered}"
            );
        }
    }
}
