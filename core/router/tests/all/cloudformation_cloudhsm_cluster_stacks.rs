// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::CloudHSM::Cluster` really creates the cluster
//! in CloudHSM, publishes what that service holds for it, and deletes it there again — while a
//! declared property value outside the pattern the type's schema states for it fails the
//! operation before anything is provisioned.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and
//! CloudFormation deploys the declaration by calling the service that OWNS the type — so the
//! identity and every read-only attribute the stack publishes for the resource are the ones
//! that service reports for it, the stack-level tags reach it beside the declaration's own, and
//! deleting the stack deletes it there; a property value outside the pattern that type's schema
//! publishes is refused by the pre-deployment validation instead, which rolls the stack back
//! with a `VALIDATION_ERROR` naming the value, the pattern and the property's own path and
//! leaves the owning service untouched.**
//!
//! Three crates have to agree for that to hold (CloudFormation submits the template and makes
//! the calls, CloudHSM serves them, and Amazon EC2 answers for the subnets a cluster is placed
//! in), so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the deploy table, along how many subnets the declaration places the cluster in and in how
//!   many Availability Zones, whether it states the `Mode` the cluster runs in or leaves that
//!   service's default, whether it carries `Tags` of its own beside the stack's, and which
//!   Region the stack is deployed in;
//! * the validation table, along the KIND of value that violates the published `HsmType`
//!   pattern — text that names no HSM family at all, a family with no size, a value spelled in
//!   the wrong case and one that is right but for a trailing space — beside the control row,
//!   whose value the pattern admits and whose cluster is really created.
//!
//! Every expected value is computed from what the row declares, from the Region each request is
//! signed for and from the account every ARN this emulator mints carries, or read from the
//! OWNING service's own report of the cluster — never out of the CloudFormation answer being
//! checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a case waits for a stack operation to reach a terminal status before the last
/// reading it took is the one asserted on. Generous on purpose: what is waited for is the
/// CONDITION, so a case takes only as long as the stack does and a loaded host cannot decide
/// the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look at a clock-owed condition waits before the next.
const POLL: Duration = Duration::from_millis(50);

/// The resource type this suite deploys, as the "AWS resource and property types reference"
/// publishes it.
const CLOUDHSM_CLUSTER: &str = "AWS::CloudHSM::Cluster";

/// The logical ids every template below declares its resources under.
const VPC: &str = "Vpc";
const CLUSTER: &str = "Cluster";

/// The HSM type every cluster that is really created below is made with — the one
/// `CreateCluster$HsmType`'s own doc publishes that CloudHSM still provisions ("The allowed
/// values are hsm1.medium and hsm2m.medium").
const HSM_TYPE: &str = "hsm2m.medium";

/// The `ClusterState` CloudHSM reports a created, uninitialized cluster in, the one it reports
/// a deleted cluster in, and the `BackupPolicy` every cluster carries (the one value that enum
/// models).
const UNINITIALIZED: &str = "UNINITIALIZED";
const DELETED: &str = "DELETED";
const BACKUP_POLICY: &str = "DEFAULT";

/// The `ClusterMode` a cluster gets when the declaration states none: CloudHSM clusters are
/// FIPS-mode unless the caller asks otherwise.
const DEFAULT_MODE: &str = "FIPS";

/// The terminal stack statuses the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `EventType` a pre-deployment validation result is reported under, the name
/// CloudFormation gives the validation that holds a declaration's properties to its type's
/// contract, and the status and failure mode a failure of it carries.
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";
const VALIDATION_FAILED: &str = "FAILED";
const VALIDATION_FAILURE_MODE: &str = "FAIL";

/// The pattern the schema of `AWS::CloudHSM::Cluster` states for `HsmType`: the form CloudHSM
/// publishes for `CreateCluster$HsmType`
/// (`smithy_query(query="operation",operation="CreateCluster")` on that service's model).
const HSM_TYPE_PATTERN: &str = r"^((p|)hsm[0-9][a-z.]*\.[a-zA-Z]+)$";

/// The tag key every row records its own stack under, so a cluster can be traced back to the
/// declaration that asked for it without reading any identifier out of the answer.
const SUITE_TAG: &str = "suite";

/// The namespace of the tags CloudFormation records on a resource of a type whose schema states
/// that it takes them — which this type's does not, so no tag of a cluster is in it.
const RESERVED_TAG_PREFIX: &str = "aws:";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process call CloudFormation makes signs its own request either way (the
/// credential scope is what routes it).
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
        service_com_amazonaws_cloudhsmv2_baldrapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` in `region`, which is
/// what routes the request and what each handler reads its Region from.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=deadbeef"
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
    (
        status,
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Call one CloudHSM operation over its awsJson1_1 wire, signed for `region`. The service names
/// itself in `X-Amz-Target` (`BaldrApiService`), which is what tells it apart from CloudHSM
/// Classic behind the one `cloudhsm` signing name.
async fn cloudhsm(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudhsmv2.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("BaldrApiService.{operation}"))
        .header("authorization", authorization("cloudhsm", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Read `stack` until it reports a status that is no longer in progress, and answer that read's
/// body. The condition is polled for, so a case takes only as long as the stack does.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
) -> String {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks({stack:?}) must answer 200, got {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {stack:?} stayed {reported:?} rather than settling"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource failure the stack recorded, so a row that did not reach the status it must
/// quotes why.
async fn reported_failures(router: &MultiServiceRouter, region: &str, stack: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, region, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|event| {
            let status = elements(event, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical = elements(event, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(event, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {status}: {reason}"))
        })
        .collect()
}

/// Every validation result the operation's own events carry, as the fields the detail is made
/// of — in the order the assertions below state them.
async fn validation_results(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, Vec<Vec<String>>) {
    let (_, body) = cloudformation(router, region, "DescribeEvents", &[("StackName", stack)]).await;
    let results = members_of(&body)
        .into_iter()
        .filter(|event| !elements(event, "ValidationName").is_empty())
        .map(|event| {
            [
                "LogicalResourceId",
                "ResourceType",
                "EventType",
                "ValidationName",
                "ValidationStatus",
                "ValidationFailureMode",
                "ValidationStatusReason",
                "ValidationPath",
            ]
            .iter()
            .map(|field| {
                elements(event, field)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            })
            .collect()
        })
        .collect();
    (body, results)
}

/// How CloudFormation reports ONE property value outside the pattern its type's schema states
/// for the property.
fn pattern_reason(value: &str, pattern: &str) -> String {
    format!("Property value [{value}] does not match pattern: {pattern}")
}

/// The clusters CloudHSM reports, narrowed by `filters` — its own read, which is the authority
/// on what a stack really created.
async fn described_clusters(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    filters: Value,
) -> Vec<Value> {
    let (status, answered) =
        cloudhsm(router, region, "DescribeClusters", &json!({ "Filters": filters })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeClusters must answer 200, got {status} {answered}"
    );
    answered["Clusters"].as_array().cloned().unwrap_or_default()
}

/// One cluster's tag set as the map a client reads it as.
fn tags_of(cluster: &Value) -> BTreeMap<String, String> {
    cluster["TagList"]
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
        .collect()
}

/// One cluster's `SubnetMapping` as the Availability-Zone-to-subnet map that service reports.
fn subnet_mapping_of(cluster: &Value) -> BTreeMap<String, String> {
    cluster["SubnetMapping"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(zone, subnet)| Some((zone, subnet.as_str()?.to_string())))
        .collect()
}

/// The text member `member` of a described cluster.
fn member(cluster: &Value, member: &str) -> String {
    cluster[member].as_str().unwrap_or_default().to_string()
}

/// One row of the deploy table.
struct Deploy {
    case: &'static str,
    stack: &'static str,
    region: &'static str,
    /// The Availability Zone each declared subnet is placed in, which is also the key the
    /// cluster's own `SubnetMapping` must report that subnet under.
    zones: &'static [&'static str],
    /// The `Mode` the declaration states, or `None` to leave CloudHSM's default.
    mode: Option<&'static str>,
    /// The tags the declaration carries of its own, beside the stack-level ones below.
    declared_tags: &'static [(&'static str, &'static str)],
    /// The tags the stack operation supplies, which CloudFormation propagates to the cluster.
    stack_tags: &'static [(&'static str, &'static str)],
}

/// The logical id — and output key — the subnet in zone `index` is declared under.
fn subnet_id(index: usize) -> String {
    format!("Subnet{index}")
}

/// The template one deploy row submits: the network the cluster is placed in, the cluster, and
/// every published return value read back as an output.
fn deploy_template(row: &Deploy) -> String {
    let mut resources = json!({
        VPC: { "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.70.0.0/16" } },
    });
    let mut subnets = Vec::new();
    for (index, zone) in row.zones.iter().enumerate() {
        let logical = subnet_id(index);
        resources[&logical] = json!({
            "Type": "AWS::EC2::Subnet",
            "Properties": {
                "VpcId": { "Ref": VPC },
                "CidrBlock": format!("10.70.{}.0/24", index + 1),
                "AvailabilityZone": zone,
            },
        });
        subnets.push(json!({ "Ref": logical }));
    }
    let mut properties = json!({ "HsmType": HSM_TYPE, "SubnetIds": subnets });
    if let Some(mode) = row.mode {
        properties["Mode"] = json!(mode);
    }
    if !row.declared_tags.is_empty() {
        properties["Tags"] = json!(row
            .declared_tags
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect::<Vec<Value>>());
    }
    resources[CLUSTER] = json!({ "Type": CLOUDHSM_CLUSTER, "Properties": properties });

    let mut outputs = json!({
        "NetworkVpcId": { "Value": { "Ref": VPC } },
        "ClusterRef": { "Value": { "Ref": CLUSTER } },
        "ClusterArn": { "Value": { "Fn::GetAtt": [CLUSTER, "Arn"] } },
        "ClusterId": { "Value": { "Fn::GetAtt": [CLUSTER, "ClusterId"] } },
        "ClusterState": { "Value": { "Fn::GetAtt": [CLUSTER, "State"] } },
        "ClusterVpcId": { "Value": { "Fn::GetAtt": [CLUSTER, "VpcId"] } },
        "ClusterSecurityGroup": { "Value": { "Fn::GetAtt": [CLUSTER, "SecurityGroup"] } },
        "ClusterBackupPolicy": { "Value": { "Fn::GetAtt": [CLUSTER, "BackupPolicy"] } },
    });
    for index in 0..row.zones.len() {
        let logical = subnet_id(index);
        outputs[&logical] = json!({ "Value": { "Ref": logical } });
    }
    json!({ "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": resources,
            "Outputs": outputs })
    .to_string()
}

/// `CreateStack` for one deploy row, with the stack-level tags it supplies.
async fn create_stack(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    stack: &str,
    template: &str,
    stack_tags: &[(&str, &str)],
) {
    let mut members: Vec<(String, String)> = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), template.to_string()),
    ];
    for (index, (key, value)) in stack_tags.iter().enumerate() {
        let at = index + 1;
        members.push((format!("Tags.member.{at}.Key"), (*key).to_string()));
        members.push((format!("Tags.member.{at}.Value"), (*value).to_string()));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, answered) = cloudformation(router, region, "CreateStack", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack:?}) must answer 200 — the template declares only \
         types AWS publishes — got {status} {answered}"
    );
}

#[tokio::test]
async fn a_declared_cloudhsm_cluster_is_created_in_cloudhsm() {
    let rows: &[Deploy] = &[
        Deploy {
            case: "one subnet, the mode the declaration states, tags of its own",
            stack: "cloudhsm-stack-one-subnet",
            region: "us-east-1",
            zones: &["us-east-1a"],
            mode: Some("NON_FIPS"),
            declared_tags: &[(SUITE_TAG, "cloudhsm-stack-one-subnet"), ("tier", "crypto")],
            stack_tags: &[("owner", "platform")],
        },
        Deploy {
            case: "two subnets in two zones, no mode at all, only the stack's own tags",
            stack: "cloudhsm-stack-two-subnets",
            region: "us-east-1",
            zones: &["us-east-1a", "us-east-1b"],
            mode: None,
            declared_tags: &[],
            stack_tags: &[(SUITE_TAG, "cloudhsm-stack-two-subnets")],
        },
        Deploy {
            case: "another Region, three subnets, and a declared tag the stack also states",
            stack: "cloudhsm-stack-eu",
            region: "eu-west-1",
            zones: &["eu-west-1a", "eu-west-1b", "eu-west-1c"],
            mode: Some("FIPS"),
            declared_tags: &[(SUITE_TAG, "cloudhsm-stack-eu"), ("owner", "the template")],
            stack_tags: &[("owner", "the stack")],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = build_router();
        let template = deploy_template(row);
        create_stack(
            &router,
            row.region,
            case,
            row.stack,
            &template,
            row.stack_tags,
        )
        .await;

        let described = settled_stack(&router, row.region, case, row.stack).await;
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        if settled != CREATE_COMPLETE {
            let failures = reported_failures(&router, row.region, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle {CREATE_COMPLETE}, got {settled:?}; \
                 the failures it reported: {failures:?}; last answer {described}"
            );
        }
        let stack_id = opt_element(&described, "StackId").unwrap_or_default();
        let outputs = outputs_of(&described);

        // What the OWNING service holds is the authority: exactly one cluster in the network
        // the stack created, read through CloudHSM's own narrowing filter.
        let network = outputs
            .get("NetworkVpcId")
            .cloned()
            .unwrap_or_else(|| "<no NetworkVpcId output>".to_string());
        let clusters = described_clusters(
            &router,
            row.region,
            case,
            json!({ "vpcIds": [network.clone()] }),
        )
        .await;
        assert_eq!(
            clusters.len(),
            1,
            "case {case:?}: expected exactly one cluster in the stack's VPC {network:?}, got \
             {clusters:?}"
        );
        let cluster = &clusters[0];
        let cluster_id = member(cluster, "ClusterId");
        let expected_arn =
            format!("arn:aws:cloudhsm:{}:{ACCOUNT}:cluster/{cluster_id}", row.region);

        // The configuration the declaration asked for is the cluster's own.
        assert_eq!(
            (
                member(cluster, "HsmType"),
                member(cluster, "Mode"),
                member(cluster, "State"),
                member(cluster, "VpcId"),
                member(cluster, "BackupPolicy"),
            ),
            (
                HSM_TYPE.to_string(),
                row.mode.unwrap_or(DEFAULT_MODE).to_string(),
                UNINITIALIZED.to_string(),
                network.clone(),
                BACKUP_POLICY.to_string(),
            ),
            "case {case:?}: expected the cluster the declaration asked for, got {cluster:?}"
        );

        // ...placed in every subnet the declaration named, under the zone each was declared in.
        let expected_mapping: BTreeMap<String, String> = row
            .zones
            .iter()
            .enumerate()
            .map(|(index, zone)| {
                (
                    (*zone).to_string(),
                    outputs
                        .get(&subnet_id(index))
                        .cloned()
                        .unwrap_or_else(|| format!("<no {} output>", subnet_id(index))),
                )
            })
            .collect();
        assert_eq!(
            subnet_mapping_of(cluster),
            expected_mapping,
            "case {case:?}: expected the cluster placed in the stack's subnets \
             {expected_mapping:?}, got {cluster:?}"
        );

        // ...carrying the tags the declaration asks for and the stack's own on every key the
        // declaration does not use — and NONE of the reserved `aws:` namespace: the type's
        // schema states that CloudFormation records no system tag of its own on a resource of
        // it, so the cluster carries exactly the tags a client asked for.
        let mut expected_tags: BTreeMap<String, String> = row
            .declared_tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        for (key, value) in row.stack_tags {
            expected_tags
                .entry((*key).to_string())
                .or_insert_with(|| (*value).to_string());
        }
        assert_eq!(
            tags_of(cluster),
            expected_tags,
            "case {case:?}: expected the cluster to carry {expected_tags:?}, got {cluster:?}"
        );
        let reserved: Vec<String> = tags_of(cluster)
            .into_keys()
            .filter(|key| key.starts_with(RESERVED_TAG_PREFIX))
            .collect();
        assert!(
            reserved.is_empty(),
            "case {case:?}: expected no tag of the reserved namespace on the cluster, got \
             {reserved:?}"
        );

        // ...and every value the STACK publishes for the resource is the one that service
        // reports, with `Ref` and `Fn::GetAtt Arn` both the cluster's ARN.
        let published: BTreeMap<&str, String> = [
            ("ClusterRef", expected_arn.clone()),
            ("ClusterArn", expected_arn.clone()),
            ("ClusterId", cluster_id.clone()),
            ("ClusterState", member(cluster, "State")),
            ("ClusterVpcId", member(cluster, "VpcId")),
            ("ClusterSecurityGroup", member(cluster, "SecurityGroup")),
            ("ClusterBackupPolicy", member(cluster, "BackupPolicy")),
        ]
        .into_iter()
        .collect();
        for (key, expected) in &published {
            assert_eq!(
                outputs.get(*key),
                Some(expected),
                "case {case:?}: expected the stack's {key} output to be {expected:?}, got \
                 {outputs:?}"
            );
        }

        // DELETING THE STACK deletes the cluster in CloudHSM: a cluster the stack no longer
        // records must not be left serving. The cluster's record survives its deletion as the
        // documented terminal-state tombstone, so what is polled for is the cluster leaving
        // every state in which it serves — never a span of the clock.
        let (status, answered) = cloudformation(
            &router,
            row.region,
            "DeleteStack",
            &[("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack must answer 200, got {status} {answered}"
        );
        let described = settled_stack(&router, row.region, case, &stack_id).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some(DELETE_COMPLETE),
            "case {case:?}: expected the delete to complete, got {described}"
        );
        // The stack holds its delete open until CloudHSM reports the cluster DELETED, so that
        // is the state it is in by now — or the record is gone, its terminal-state tombstone
        // having been reclaimed by the store that keeps it. What is NOT admitted is any state
        // in which the cluster still serves, or one it is still being taken down in.
        let reported = described_clusters(
            &router,
            row.region,
            case,
            json!({ "clusterIds": [cluster_id.clone()] }),
        )
        .await;
        let state = reported.first().map(|cluster| member(cluster, "State"));
        assert!(
            state.as_deref().is_none_or(|state| state == DELETED),
            "case {case:?}: expected the cluster {cluster_id} the deleted stack created to be \
             {DELETED} in CloudHSM, it is reported {state:?}"
        );
    }
}

/// One row of the validation table: the `HsmType` the declaration carries, and whether the
/// pattern the type's schema states admits it.
struct Validate {
    case: &'static str,
    stack: &'static str,
    hsm_type: &'static str,
    admitted: bool,
}

#[tokio::test]
async fn a_cluster_property_outside_its_schemas_pattern_rolls_the_stack_back() {
    const REGION: &str = "us-east-1";
    let rows: &[Validate] = &[
        Validate {
            case: "an HSM type that names no HSM family at all",
            stack: "cloudhsm-bad-type",
            hsm_type: "not-a-valid-hsm-type",
            admitted: false,
        },
        Validate {
            case: "an HSM family with no size",
            stack: "cloudhsm-no-size",
            hsm_type: "hsm2m",
            admitted: false,
        },
        Validate {
            case: "an HSM type spelled in the wrong case",
            stack: "cloudhsm-wrong-case",
            hsm_type: "HSM2M.MEDIUM",
            admitted: false,
        },
        Validate {
            case: "an HSM type the form admits but for a trailing space",
            stack: "cloudhsm-trailing-space",
            hsm_type: "hsm2m.medium ",
            admitted: false,
        },
        Validate {
            case: "the HSM type the form admits",
            stack: "cloudhsm-good-type",
            hsm_type: HSM_TYPE,
            admitted: true,
        },
    ];

    let router = build_router();
    for row in rows {
        let case = row.case;
        let template = json!({
            "Resources": {
                VPC: { "Type": "AWS::EC2::VPC", "Properties": { "CidrBlock": "10.71.0.0/16" } },
                "Subnet0": {
                    "Type": "AWS::EC2::Subnet",
                    "Properties": {
                        "VpcId": { "Ref": VPC },
                        "CidrBlock": "10.71.1.0/24",
                        "AvailabilityZone": format!("{REGION}a"),
                    },
                },
                CLUSTER: {
                    "Type": CLOUDHSM_CLUSTER,
                    "Properties": {
                        "HsmType": row.hsm_type,
                        "SubnetIds": [{ "Ref": "Subnet0" }],
                        "Tags": [{ "Key": SUITE_TAG, "Value": row.stack }],
                    },
                },
            },
        })
        .to_string();
        create_stack(&router, REGION, case, row.stack, &template, &[]).await;

        let expected_status = if row.admitted {
            CREATE_COMPLETE
        } else {
            ROLLBACK_COMPLETE
        };
        let described = settled_stack(&router, REGION, case, row.stack).await;
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        if settled != expected_status {
            let failures = reported_failures(&router, REGION, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle {expected_status}, got {settled:?}; \
                 the failures it reported: {failures:?}; last answer {described}"
            );
        }

        // The DETAIL of the refusal, on the operation's own events: which validation failed, on
        // which resource, the value and the pattern it does not match, at the path of the
        // property that was judged.
        let (body, results) = validation_results(&router, REGION, row.stack).await;
        let expected: Vec<Vec<String>> = if row.admitted {
            Vec::new()
        } else {
            vec![vec![
                CLUSTER.to_string(),
                CLOUDHSM_CLUSTER.to_string(),
                VALIDATION_ERROR.to_string(),
                PROPERTY_VALIDATION.to_string(),
                VALIDATION_FAILED.to_string(),
                VALIDATION_FAILURE_MODE.to_string(),
                pattern_reason(row.hsm_type, HSM_TYPE_PATTERN),
                format!("/Resources/{CLUSTER}/Properties/HsmType"),
            ]]
        };
        assert_eq!(
            results, expected,
            "case {case:?}: expected the validation results {expected:?}, got {results:?} in \
             {body}"
        );

        // ...and CloudHSM is the proof that nothing was attempted: no cluster carries the
        // rejected declaration's own tag, while the admitted one's cluster does.
        let carrying: Vec<Value> = described_clusters(&router, REGION, case, json!({}))
            .await
            .into_iter()
            .filter(|cluster| tags_of(cluster).get(SUITE_TAG).map(String::as_str) == Some(row.stack))
            .collect();
        assert_eq!(
            carrying.len(),
            usize::from(row.admitted),
            "case {case:?}: expected {} cluster carrying the declaration's own tag, got \
             {carrying:?}",
            usize::from(row.admitted)
        );
    }
}
