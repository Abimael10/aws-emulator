// SPDX-License-Identifier: CC0-1.0

//! The Amazon Redshift Serverless estate a stack declares, end to end: the namespace, the
//! workgroup of one and the snapshot of one all exist in that service itself under the names
//! the stack published, the values the types publish are the ones it reported, an update that
//! drops the children takes them away while the namespace survives, deleting the stack takes
//! the whole estate — endpoint network interfaces included — and a declaration that service
//! REFUSES fails the resource with its own refusal and rolls the stack back.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may declare, and a
//! declaration of it is DEPLOYED by calling the service that owns the type — so the handle the
//! stack publishes (`Ref`) resolves in that service, the values the type publishes (`Fn::GetAtt`)
//! are the ones that service reported, and what that service refuses is a FAILED RESOURCE rolled
//! back rather than a template-format error.
//!
//! The rows of the deploy table differ along what the rule generalises over: the base capacity
//! the workgroup is given (the smallest standard size, a larger one and one of the two tiny
//! sizes), how many subnets the workgroup's endpoint is placed in (one, two, three), whether the
//! workgroup is publicly reachable, whether the stack also declares a SNAPSHOT of the namespace
//! and the retention it asks for, and which published return value each output reads (`Ref`,
//! `Fn::GetAtt Namespace.NamespaceId` / `Namespace.NamespaceArn` / `Workgroup.WorkgroupId` /
//! `Workgroup.WorkgroupArn` / `Workgroup.Status` / `Snapshot.SnapshotArn`).
//!
//! The refusal rows differ along WHICH size in the tiny RPU range the declaration asks for —
//! every one of them a size that service does not serve. Each is the owning service's own
//! refusal, quoted on the resource that failed, never the `Template format error: Unrecognized
//! resource types` a type CloudFormation does not know is refused with, which no row of any
//! table here may ever see.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds
//! — the CloudFormation calls over its awsQuery wire, the Redshift Serverless calls over that
//! service's own `awsJson1_1` wire — so what is asserted is what a client that deploys the
//! template and then calls redshift-serverless observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_text,
    REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, output_value};

/// The three resource types under test.
const NAMESPACE: &str = "AWS::RedshiftServerless::Namespace";
const WORKGROUP: &str = "AWS::RedshiftServerless::Workgroup";
const SNAPSHOT: &str = "AWS::RedshiftServerless::Snapshot";

/// The two Amazon EC2 network types the estate is placed in, and the group governing it.
const VPC: &str = "AWS::EC2::VPC";
const SUBNET: &str = "AWS::EC2::Subnet";
const SECURITY_GROUP: &str = "AWS::EC2::SecurityGroup";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of any table here may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The status all three resources are usable in (`NamespaceStatus`, `WorkgroupStatus` and
/// `SnapshotStatus`: `smithy_query(query="shape",
/// shape="NamespaceStatus,WorkgroupStatus,SnapshotStatus",
/// model_path="services/service-com-amazonaws-redshiftserverless-redshiftserverless/\
/// smithy-model/com-amazonaws-redshiftserverless-redshiftserverless.json")`).
const AVAILABLE: &str = "AVAILABLE";

/// The error code Amazon Redshift Serverless answers for a resource it does not hold — the
/// modelled 404 of `GetNamespace`, `GetWorkgroup` and `GetSnapshot`.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The status it answers a resource-NAME refusal with: a namespace and a workgroup are
/// addressed by name, which this service refuses with 400 rather than with the error shape's own
/// `@httpError`, while a SNAPSHOT's miss carries the modelled 404.
const NAME_REFUSAL_STATUS: u16 = 400;
const NOT_FOUND_STATUS: u16 = 404;

/// The smallest STANDARD base capacity that service serves, and one of the two TINY sizes it
/// serves below it. Source: the sentence it words for a tiny size it does not serve, "The base
/// capacity must be 2 or 4 in tiny RPU range."
const SMALLEST_STANDARD_BASE_CAPACITY: i64 = 8;
const A_TINY_BASE_CAPACITY: i64 = 4;

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The Availability Zones the estate's subnets are placed in, one per subnet so the workgroup's
/// endpoint takes an interface in each ("You can specify only one subnet per Availability Zone"
/// for an interface endpoint).
const ZONES: [&str; 3] = ["us-east-1a", "us-east-1b", "us-east-1c"];

/// One Amazon Redshift Serverless call over the `awsJson1_1` wire its model declares, on the
/// `POST /` every operation of that protocol binds to, signed for `redshift-serverless` — that
/// model's own `sigv4Name` — in the Region the stack's resources live in.
async fn redshift_serverless(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://redshift-serverless.{REGION}.amazonaws.com/"))
        .header("host", format!("redshift-serverless.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("RedshiftServerless.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/redshift-serverless/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed Amazon Redshift Serverless request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// That service's own read of one namespace, by the name the stack published.
async fn get_namespace(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    redshift_serverless(router, "GetNamespace", &json!({ "namespaceName": name })).await
}

/// Its own read of one workgroup, by the name the stack published.
async fn get_workgroup(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    redshift_serverless(router, "GetWorkgroup", &json!({ "workgroupName": name })).await
}

/// Its own read of one snapshot, by the name the stack published.
async fn get_snapshot(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    redshift_serverless(router, "GetSnapshot", &json!({ "snapshotName": name })).await
}

/// The error code out of one awsJson error envelope, with any namespace prefix dropped.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split('#')
        .next_back()
        .unwrap_or_default()
        .to_string()
}

/// The message out of one awsJson error envelope.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Submit `template` as `stack_name`, carrying the stack's own tag, and answer the `CreateStack`
/// body: a type Amazon Web Services publishes is ADMITTED, so this never sees the
/// template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — a {WORKGROUP} is a type a template \
         may declare — got {status} with body {body}"
    );
    body
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// The output `key` the described stack reports, or a panic naming the case.
fn published(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
}

/// Read one record until that service reports it `AVAILABLE`, or answer the last read once the
/// deadline passes: the status is one the CLOCK makes due, so what is waited for is the
/// CONDITION rather than a span of wall clock.
async fn available(
    router: &MultiServiceRouter,
    case: &str,
    reading: Reading,
    name: &str,
) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, answered) = reading.read(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon Redshift Serverless to report the {reading:?} the \
             stack created under {name:?}, got {status} with {answered}"
        );
        let record = answered
            .get(reading.member())
            .cloned()
            .unwrap_or(Value::Null);
        let reported = record
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reported == AVAILABLE || std::time::Instant::now() >= deadline {
            return record;
        }
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Which of the three resources a read is asking about.
#[derive(Debug, Clone, Copy)]
enum Reading {
    Namespace,
    Workgroup,
    Snapshot,
}

impl Reading {
    /// That service's own read of this kind of record, by the name the stack published.
    async fn read(self, router: &MultiServiceRouter, name: &str) -> (u16, Value) {
        match self {
            Self::Namespace => get_namespace(router, name).await,
            Self::Workgroup => get_workgroup(router, name).await,
            Self::Snapshot => get_snapshot(router, name).await,
        }
    }

    /// The member its answer carries the record under.
    fn member(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Workgroup => "workgroup",
            Self::Snapshot => "snapshot",
        }
    }

    /// The status this service refuses a read of a record it does not hold with: a namespace and
    /// a workgroup are addressed by NAME, which it refuses with [`NAME_REFUSAL_STATUS`], while a
    /// snapshot's miss carries the modelled [`NOT_FOUND_STATUS`].
    fn missing_status(self) -> u16 {
        match self {
            Self::Namespace | Self::Workgroup => NAME_REFUSAL_STATUS,
            Self::Snapshot => NOT_FOUND_STATUS,
        }
    }
}

/// What one row of the deploy table declares and what the estate must come up as.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The names the declaration states, which are the handles the stack publishes.
    namespace_name: &'static str,
    workgroup_name: &'static str,
    /// The snapshot the row declares of the namespace, with the retention it asks for.
    snapshot: Option<(&'static str, i64)>,
    /// The compute the workgroup is given.
    base_capacity: i64,
    /// How many of the stack's own subnets the workgroup's endpoint is placed in.
    subnets: usize,
    /// Whether the workgroup is reachable from a public network.
    publicly_accessible: bool,
}

/// The rows differ along the base capacity (the smallest standard size, a larger one, one of the
/// tiny sizes), how many subnets the endpoint is placed in, whether the workgroup is publicly
/// reachable and whether the stack also declares a snapshot — and, through [`estate_template`],
/// which published return values the outputs read.
const ROWS: &[Row] = &[
    // The issue's own stack: the whole estate, every return value read back.
    Row {
        case: "the whole estate at the smallest standard base capacity",
        stack_name: "rss-estate-round-trip",
        namespace_name: "rss-round-trip-ns",
        workgroup_name: "rss-round-trip-wg",
        snapshot: Some(("rss-round-trip-snap", 1)),
        base_capacity: SMALLEST_STANDARD_BASE_CAPACITY,
        subnets: 3,
        publicly_accessible: false,
    },
    // A larger standard base capacity, a publicly reachable workgroup, one subnet and a longer
    // snapshot retention.
    Row {
        case: "a larger base capacity, publicly reachable, one subnet",
        stack_name: "rss-estate-public",
        namespace_name: "rss-public-ns",
        workgroup_name: "rss-public-wg",
        snapshot: Some(("rss-public-snap", 30)),
        base_capacity: SMALLEST_STANDARD_BASE_CAPACITY * 4,
        subnets: 1,
        publicly_accessible: true,
    },
    // One of the two TINY sizes that service serves, two subnets, and NO snapshot at all.
    Row {
        case: "a tiny base capacity and no snapshot",
        stack_name: "rss-estate-tiny",
        namespace_name: "rss-tiny-ns",
        workgroup_name: "rss-tiny-wg",
        snapshot: None,
        base_capacity: A_TINY_BASE_CAPACITY,
        subnets: 2,
        publicly_accessible: false,
    },
];

/// The template one row declares: an Amazon EC2 VPC, `subnets` subnets of it, a security group
/// of it, the namespace, the workgroup placed in that network and — where the row asks for one —
/// a snapshot of the namespace taken once the workgroup is up.
///
/// The workgroup names its namespace, its subnets and its security group with `Ref`, so what
/// reaches Amazon Redshift Serverless is the name the stack published for the namespace and the
/// identifiers Amazon EC2 minted for the network.
fn estate_template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Vpc".to_string(),
        json!({ "Type": VPC, "Properties": { "CidrBlock": "10.74.0.0/16" } }),
    );
    let mut subnet_refs = Vec::new();
    for index in 0..row.subnets {
        let logical_id = format!("Subnet{index}");
        resources.insert(
            logical_id.clone(),
            json!({
                "Type": SUBNET,
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": format!("10.74.{}.0/24", index + 1),
                    "AvailabilityZone": ZONES[index % ZONES.len()],
                }
            }),
        );
        subnet_refs.push(json!({ "Ref": logical_id }));
    }
    resources.insert(
        "Group".to_string(),
        json!({
            "Type": SECURITY_GROUP,
            "Properties": {
                "GroupDescription": "the estate compute",
                "VpcId": { "Ref": "Vpc" },
            }
        }),
    );
    resources.insert(
        "Namespace".to_string(),
        json!({
            "Type": NAMESPACE,
            "Properties": {
                "NamespaceName": row.namespace_name,
                "DbName": "analytics",
                "AdminUsername": "estateadmin",
                "Tags": [{ "Key": "sim2real-rss", "Value": "namespace" }],
            }
        }),
    );
    resources.insert(
        "Workgroup".to_string(),
        json!({
            "Type": WORKGROUP,
            "Properties": {
                "WorkgroupName": row.workgroup_name,
                "NamespaceName": { "Ref": "Namespace" },
                "BaseCapacity": row.base_capacity,
                "SubnetIds": subnet_refs,
                "SecurityGroupIds": [{ "Ref": "Group" }],
                "PubliclyAccessible": row.publicly_accessible,
                "Tags": [{ "Key": "sim2real-rss", "Value": "workgroup" }],
            }
        }),
    );
    let mut outputs = json!({
        "NamespaceRef": { "Value": { "Ref": "Namespace" } },
        "NamespaceId": { "Value": { "Fn::GetAtt": ["Namespace", "Namespace.NamespaceId"] } },
        "NamespaceArn": { "Value": { "Fn::GetAtt": ["Namespace", "Namespace.NamespaceArn"] } },
        "WorkgroupRef": { "Value": { "Ref": "Workgroup" } },
        "WorkgroupId": { "Value": { "Fn::GetAtt": ["Workgroup", "Workgroup.WorkgroupId"] } },
        "WorkgroupArn": { "Value": { "Fn::GetAtt": ["Workgroup", "Workgroup.WorkgroupArn"] } },
        "WorkgroupStatus": { "Value": { "Fn::GetAtt": ["Workgroup", "Workgroup.Status"] } },
    });
    if let Some((snapshot_name, retention)) = row.snapshot {
        resources.insert(
            "Snapshot".to_string(),
            json!({
                "Type": SNAPSHOT,
                "DependsOn": "Workgroup",
                "Properties": {
                    "SnapshotName": snapshot_name,
                    "NamespaceName": { "Ref": "Namespace" },
                    "RetentionPeriod": retention,
                }
            }),
        );
        let entries = outputs.as_object_mut().expect("the outputs are an object");
        entries.insert(
            "SnapshotRef".to_string(),
            json!({ "Value": { "Ref": "Snapshot" } }),
        );
        entries.insert(
            "SnapshotArn".to_string(),
            json!({ "Value": { "Fn::GetAtt": ["Snapshot", "Snapshot.SnapshotArn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": outputs,
    })
    .to_string()
}

#[tokio::test]
async fn a_stack_deploys_the_redshift_serverless_estate() {
    for row in ROWS {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &estate_template(row)).await;

        // `Ref` resolves to the NAME each type publishes, which is the handle every call of the
        // owning service on the resource takes.
        assert_eq!(
            published(case, &described, "NamespaceRef"),
            row.namespace_name,
            "case {case:?}: expected the namespace's Ref to be the name it was created under, \
             got {described}"
        );
        assert_eq!(
            published(case, &described, "WorkgroupRef"),
            row.workgroup_name,
            "case {case:?}: expected the workgroup's Ref to be the name it was created under, \
             got {described}"
        );

        // The namespace exists in the owning service under that name, with the first database
        // and the administrator the declaration asked for.
        let namespace = available(&router, case, Reading::Namespace, row.namespace_name).await;
        assert_eq!(
            (
                namespace.get("namespaceName").and_then(Value::as_str),
                namespace.get("dbName").and_then(Value::as_str),
                namespace.get("adminUsername").and_then(Value::as_str),
                namespace.get("status").and_then(Value::as_str),
            ),
            (
                Some(row.namespace_name),
                Some("analytics"),
                Some("estateadmin"),
                Some(AVAILABLE),
            ),
            "case {case:?}: expected the namespace the declaration asked for, got {namespace}"
        );

        // And the values the type publishes are the ones that service reported for it.
        for (key, member) in [
            ("NamespaceId", "namespaceId"),
            ("NamespaceArn", "namespaceArn"),
        ] {
            let reported = namespace
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                published(case, &described, key),
                reported,
                "case {case:?}: expected the {key} output to be the {member} that service \
                 reported, got {described}"
            );
        }

        // The workgroup exists under its own name, serving that namespace, with the compute,
        // the network and the reachability the declaration asked for.
        let workgroup = available(&router, case, Reading::Workgroup, row.workgroup_name).await;
        assert_eq!(
            (
                workgroup.get("workgroupName").and_then(Value::as_str),
                workgroup.get("namespaceName").and_then(Value::as_str),
                workgroup.get("baseCapacity").and_then(Value::as_i64),
                workgroup.get("publiclyAccessible").and_then(Value::as_bool),
                workgroup.get("status").and_then(Value::as_str),
            ),
            (
                Some(row.workgroup_name),
                Some(row.namespace_name),
                Some(row.base_capacity),
                Some(row.publicly_accessible),
                Some(AVAILABLE),
            ),
            "case {case:?}: expected the workgroup the declaration asked for, got {workgroup}"
        );

        // The subnets and the security group it carries are the identifiers Amazon EC2 minted
        // for the resources of the SAME stack, which is what placing the endpoint in the
        // stack's own network means.
        let carried_subnets = identifiers(&workgroup, "subnetIds");
        assert_eq!(
            carried_subnets.len(),
            row.subnets,
            "case {case:?}: expected the workgroup to carry the stack's {} subnets, got \
             {carried_subnets:?}",
            row.subnets
        );
        assert!(
            carried_subnets
                .iter()
                .all(|subnet| subnet.starts_with("subnet-")),
            "case {case:?}: expected every subnet to be one Amazon EC2 minted, got \
             {carried_subnets:?}"
        );
        let carried_groups = identifiers(&workgroup, "securityGroupIds");
        assert!(
            carried_groups.len() == 1 && carried_groups[0].starts_with("sg-"),
            "case {case:?}: expected the stack's own security group, got {carried_groups:?}"
        );

        // The endpoint that service publishes for the workgroup stands in the VPC the stack
        // created, with one interface per subnet it is placed in.
        let interfaces = workgroup
            .get("endpoint")
            .and_then(|endpoint| endpoint.get("vpcEndpoints"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            interfaces.len(),
            1,
            "case {case:?}: expected one managed VPC endpoint for the workgroup, got \
             {interfaces:?}"
        );
        let placed = interfaces[0]
            .get("networkInterfaces")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            placed, row.subnets,
            "case {case:?}: expected one endpoint network interface per subnet, got \
             {interfaces:?}"
        );

        for (key, member) in [
            ("WorkgroupId", "workgroupId"),
            ("WorkgroupArn", "workgroupArn"),
            ("WorkgroupStatus", "status"),
        ] {
            let reported = workgroup
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                published(case, &described, key),
                reported,
                "case {case:?}: expected the {key} output to be the {member} that service \
                 reported, got {described}"
            );
        }

        // The snapshot the row declares exists too, with the retention the declaration asked
        // for, and the ARN the type publishes is the one that service built.
        match row.snapshot {
            None => assert_eq!(
                output_value(&described, "SnapshotRef"),
                None,
                "case {case:?}: expected no snapshot output for a stack declaring none, got \
                 {described}"
            ),
            Some((snapshot_name, retention)) => {
                assert_eq!(
                    published(case, &described, "SnapshotRef"),
                    snapshot_name,
                    "case {case:?}: expected the snapshot's Ref to be the name it was taken \
                     under, got {described}"
                );
                let snapshot =
                    available(&router, case, Reading::Snapshot, snapshot_name).await;
                assert_eq!(
                    (
                        snapshot.get("snapshotName").and_then(Value::as_str),
                        snapshot.get("namespaceName").and_then(Value::as_str),
                        snapshot
                            .get("snapshotRetentionPeriod")
                            .and_then(Value::as_i64),
                        snapshot.get("status").and_then(Value::as_str),
                    ),
                    (
                        Some(snapshot_name),
                        Some(row.namespace_name),
                        Some(retention),
                        Some(AVAILABLE),
                    ),
                    "case {case:?}: expected the snapshot the declaration asked for, got \
                     {snapshot}"
                );
                assert_eq!(
                    published(case, &described, "SnapshotArn"),
                    snapshot
                        .get("snapshotArn")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "case {case:?}: expected the SnapshotArn output to be the ARN that service \
                     built, got {described}"
                );
            }
        }
    }
}

/// The string entries of one reported list member.
fn identifiers(record: &Value, member: &str) -> Vec<String> {
    record
        .get(member)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One base capacity in the TINY RPU range that Amazon Redshift Serverless does not serve.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    namespace_name: &'static str,
    workgroup_name: &'static str,
    base_capacity: i64,
}

/// The rows differ along WHICH size in the tiny RPU range the declaration asks for: below the
/// smallest tiny size, between the two it serves, and above the larger one but still inside the
/// range — the issue's own case.
const REFUSED: &[Refused] = &[
    Refused {
        case: "a base capacity below the smallest tiny size",
        stack_name: "rss-capacity-one",
        namespace_name: "rss-capacity-one-ns",
        workgroup_name: "rss-capacity-one-wg",
        base_capacity: 1,
    },
    Refused {
        case: "a base capacity between the two tiny sizes",
        stack_name: "rss-capacity-three",
        namespace_name: "rss-capacity-three-ns",
        workgroup_name: "rss-capacity-three-wg",
        base_capacity: 3,
    },
    Refused {
        case: "a base capacity above the larger tiny size, still in the range",
        stack_name: "rss-capacity-seven",
        namespace_name: "rss-capacity-seven-ns",
        workgroup_name: "rss-capacity-seven-wg",
        base_capacity: SMALLEST_STANDARD_BASE_CAPACITY - 1,
    },
];

#[tokio::test]
async fn a_base_capacity_the_service_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Namespace": {
                    "Type": NAMESPACE,
                    "Properties": { "NamespaceName": row.namespace_name }
                },
                "Workgroup": {
                    "Type": WORKGROUP,
                    "Properties": {
                        "WorkgroupName": row.workgroup_name,
                        "NamespaceName": { "Ref": "Namespace" },
                        "BaseCapacity": row.base_capacity,
                    }
                }
            },
            "Outputs": { "WorkgroupRef": { "Value": { "Ref": "Workgroup" } } }
        })
        .to_string();

        // What the OWNING service answers for exactly that base capacity is what the failed
        // resource has to quote, so it is read from that service itself rather than written out
        // here.
        let (status, refusal) = redshift_serverless(
            &router,
            "CreateWorkgroup",
            &json!({
                "namespaceName": format!("{}-direct", row.namespace_name),
                "workgroupName": format!("{}-direct", row.workgroup_name),
                "baseCapacity": row.base_capacity,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected Amazon Redshift Serverless to refuse the base capacity \
             itself, got {status} with {refusal}"
        );
        let sentence = error_message(&refusal);
        assert!(
            !sentence.is_empty(),
            "case {case:?}: expected that service to word the refusal, got {refusal}"
        );

        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back, got {reached} with \
             failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "WorkgroupRef"),
            None,
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, inside
        // the envelope a resource provider reports one in — and never a refusal of the template.
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES) && !events.contains("VALIDATION_ERROR"),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let expected_envelope = "(Service: RedshiftServerless, Status Code: 400,";
        let expected_suffix = "HandlerErrorCode: InvalidRequest";
        assert!(
            reasons.iter().any(|reason| reason.contains(&sentence)
                && reason.contains(expected_envelope)
                && reason.contains("SDK Attempt Count:")
                && reason.contains(expected_suffix)),
            "case {case:?}: expected a failure reason carrying {sentence:?}, \
             {expected_envelope:?} and {expected_suffix:?}, got {reasons:?}"
        );

        // And nothing was left behind: after the rollback that service holds neither the
        // workgroup nor the namespace.
        for (reading, name) in [
            (Reading::Workgroup, row.workgroup_name),
            (Reading::Namespace, row.namespace_name),
        ] {
            let (status, answered) = reading.read(&router, name).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (reading.missing_status(), RESOURCE_NOT_FOUND),
                "case {case:?}: expected the rollback to leave no {reading:?} {name:?}, got \
                 {status} with {answered}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_that_drops_the_children_keeps_the_namespace() {
    let router = router();
    let case = "an update dropping the workgroup and the snapshot";
    let row = &ROWS[0];
    deployed(&router, case, row.stack_name, &estate_template(row)).await;
    let (snapshot_name, _) = row.snapshot.expect("the first row declares a snapshot");
    available(&router, case, Reading::Workgroup, row.workgroup_name).await;
    available(&router, case, Reading::Snapshot, snapshot_name).await;

    // The same template with the two children dropped: the namespace, the VPC, the subnets and
    // the security group stay.
    let revised = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": { "Type": VPC, "Properties": { "CidrBlock": "10.74.0.0/16" } },
            "Subnet0": {
                "Type": SUBNET,
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.74.1.0/24",
                    "AvailabilityZone": ZONES[0],
                }
            },
            "Subnet1": {
                "Type": SUBNET,
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.74.2.0/24",
                    "AvailabilityZone": ZONES[1],
                }
            },
            "Subnet2": {
                "Type": SUBNET,
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "CidrBlock": "10.74.3.0/24",
                    "AvailabilityZone": ZONES[2],
                }
            },
            "Group": {
                "Type": SECURITY_GROUP,
                "Properties": {
                    "GroupDescription": "the estate compute",
                    "VpcId": { "Ref": "Vpc" },
                }
            },
            "Namespace": {
                "Type": NAMESPACE,
                "Properties": {
                    "NamespaceName": row.namespace_name,
                    "DbName": "analytics",
                    "AdminUsername": "estateadmin",
                    "Tags": [{ "Key": "sim2real-rss", "Value": "namespace" }],
                }
            }
        },
        "Outputs": { "NamespaceRef": { "Value": { "Ref": "Namespace" } } }
    })
    .to_string();

    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", row.stack_name),
            member("TemplateBody", &revised),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
    );
    let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, row.stack_name).await
    );

    // The two children the update dropped are gone from the owning service, each answered with
    // that service's own sentence for a resource it does not hold.
    let (status, answered) = get_workgroup(&router, row.workgroup_name).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (NAME_REFUSAL_STATUS, RESOURCE_NOT_FOUND),
        "case {case:?}: expected the dropped workgroup to be gone, got {status} with {answered}"
    );
    assert_eq!(
        error_message(&answered),
        format!("Serverless workgroup {} not found.", row.workgroup_name),
        "case {case:?}: expected that service's own sentence for a workgroup it does not hold, \
         got {answered}"
    );
    let (status, answered) = get_snapshot(&router, snapshot_name).await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (NOT_FOUND_STATUS, RESOURCE_NOT_FOUND),
        "case {case:?}: expected the dropped snapshot to be gone, got {status} with {answered}"
    );
    assert!(
        error_message(&answered)
            .starts_with(&format!("The snapshot {snapshot_name} was not found.")),
        "case {case:?}: expected that service's own sentence for a snapshot it does not hold, \
         got {answered}"
    );

    // And the namespace the update kept is still there, still reported by its own name.
    let namespace = available(&router, case, Reading::Namespace, row.namespace_name).await;
    assert_eq!(
        namespace.get("namespaceName").and_then(Value::as_str),
        Some(row.namespace_name),
        "case {case:?}: expected the namespace the update kept to survive, got {namespace}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_whole_estate() {
    let router = router();
    let case = "the estate deleted with its stack";
    let row = &ROWS[0];
    let described = deployed(&router, case, row.stack_name, &estate_template(row)).await;
    // A deleted stack stops being addressable by NAME, so the teardown is read by the stack's
    // own id — which is what reports the terminal `DELETE_COMPLETE`.
    let stack_id = xml_text(&described, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: no StackId in {described}"));
    let (snapshot_name, _) = row.snapshot.expect("the first row declares a snapshot");
    available(&router, case, Reading::Workgroup, row.workgroup_name).await;
    available(&router, case, Reading::Snapshot, snapshot_name).await;

    let (status, deleted) = call(
        &router,
        "DeleteStack",
        &[member("StackName", row.stack_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
    );
    // The stack reaching DELETE_COMPLETE is itself the proof the workgroup's endpoint network
    // interfaces were released: Amazon EC2 refuses to delete a subnet that still has one in it,
    // so the subnets the stack declared could not go otherwise.
    let reached = settled_stack_status(case, &router, REGION, &stack_id).await;
    assert_eq!(
        reached, "DELETE_COMPLETE",
        "case {case:?}: expected the delete to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, &stack_id).await
    );

    for (reading, name) in [
        (Reading::Workgroup, row.workgroup_name),
        (Reading::Snapshot, snapshot_name),
        (Reading::Namespace, row.namespace_name),
    ] {
        let (status, answered) = reading.read(&router, name).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (reading.missing_status(), RESOURCE_NOT_FOUND),
            "case {case:?}: expected the {reading:?} {name:?} to go with the stack, got {status} \
             with {answered}"
        );
    }
}
