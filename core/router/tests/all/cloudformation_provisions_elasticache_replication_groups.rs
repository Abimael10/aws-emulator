// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ElastiCache::ReplicationGroup` has really
//! created that group — in Amazon ElastiCache, in the cache subnet group the same stack
//! declared — and the endpoint attributes the type publishes are the ones that service reports
//! for it, so every stack output reading one carries a value.
//!
//! The rule under test: **a stack reaches its completed status only once the service that owns
//! each declared resource has created it, through that service's own create, and every
//! attribute the type publishes is read off that service's record of the resource — so an
//! output over `Fn::GetAtt` carries the value the owning service published, never no value at
//! all**. Three crates have to agree for that to hold (CloudFormation makes the call,
//! ElastiCache serves the group, Amazon EC2 owns the subnets and security groups it is placed
//! in), so the suite lives here.
//!
//! The ACCEPTED rows differ along what the rule generalises over: whether the template names
//! the group (`ReplicationGroupId`) or leaves CloudFormation to name it, how many cache
//! clusters and shards it declares, whether its keyspace is partitioned (which is what decides
//! WHICH endpoints a group publishes), and whether it asks for in-transit encryption with an
//! auth token. Each row reads the endpoints its own topology publishes out of the stack and
//! compares them against ElastiCache's own answer.
//!
//! The REFUSED rows differ along WHICH rule of the owning service the declaration breaks, so
//! what is asserted is that the create is the owning service's own rather than that one
//! recorded template fails.
//!
//! Nothing here reaches into either service's state: a group exists only because
//! `DescribeReplicationGroups` reported it, and a subnet only because EC2 minted it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    block_of, blocks_of, element, elements, form_encode, opt_element, outputs_of,
};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The API versions each of the three wires carries beside `Action`.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
const EC2_API_VERSION: &str = "2016-11-15";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack's own clock-owed transition and the created group's
/// provisioning span both have to come due, and the suite waits for the CONDITION (the stack's
/// status, the group being gone) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The logical ids every template below declares its two resources under.
const GROUP_LOGICAL_ID: &str = "Group";
const SUBNET_GROUP_LOGICAL_ID: &str = "SubnetGroup";

/// An identifier of the EC2 security-group form that no `CreateSecurityGroup` below minted, so
/// it reaches ElastiCache's security-group resolution and resolves to nothing.
const ABSENT_SECURITY_GROUP: &str = "sg-00000000deadbeef";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticache_amazonelasticachev9::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one query-protocol operation of `service`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        action,
        members,
    )
    .await
}

/// Call one Amazon ElastiCache operation over its awsQuery wire.
async fn elasticache(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "elasticache",
        ELASTICACHE_API_VERSION,
        action,
        members,
    )
    .await
}

/// One Amazon EC2 action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The account's network for this suite, built through Amazon EC2's own operations: a VPC, one
/// subnet per zone and one security group, so every identifier a template below hands
/// ElastiCache is one the owner of those resources handed out.
struct Network {
    subnets: Vec<String>,
    security_group: String,
}

/// Build [`Network`] with one subnet per zone.
async fn network(router: &MultiServiceRouter, zones: &[&str]) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.9.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.9.{}.0/24", index + 1);
        let body = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr.as_str()),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&body, "subnetId"));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "cfn-replication-group"),
                ("GroupDescription", "groups a stack declares"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnets,
        security_group,
    }
}

/// The `"SubnetIds":[…]` / `"SecurityGroupIds":[…]` template members for these identifiers.
fn id_list(property: &str, ids: &[&str]) -> String {
    let quoted: Vec<String> = ids.iter().map(|id| format!("\"{id}\"")).collect();
    format!(r#","{property}":[{}]"#, quoted.join(","))
}

/// The description every template below gives the replication group it declares.
const GROUP_DESCRIPTION: &str = "the group a stack declares";

/// The template one row deploys: the cache subnet group the account's subnets are gathered
/// into, the replication group placed in it, and the outputs the row reads.
///
/// The group's placement is declared through `Ref` on the subnet group, which is how a template
/// states that the group belongs in the one this stack creates — so the name ElastiCache
/// resolves is the name the stack gave that group.
fn template_of(
    group_id: &str,
    group_properties: &str,
    network: &Network,
    security_groups: &[&str],
    outputs: &[(&str, &str)],
) -> String {
    let subnets: Vec<&str> = network.subnets.iter().map(String::as_str).collect();
    let mut template = String::from(r#"{"Resources":{""#);
    template.push_str(SUBNET_GROUP_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::ElastiCache::SubnetGroup","Properties":{"#);
    template.push_str(r#""Description":"the subnets a stack gathers""#);
    template.push_str(&id_list("SubnetIds", &subnets));
    template.push_str(r#"}},""#);
    template.push_str(GROUP_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::ElastiCache::ReplicationGroup","Properties":{"#);
    template.push_str(&format!(
        r#""ReplicationGroupDescription":"{GROUP_DESCRIPTION}""#
    ));
    if !group_id.is_empty() {
        template.push_str(&format!(r#","ReplicationGroupId":"{group_id}""#));
    }
    template.push_str(&format!(
        r#","CacheSubnetGroupName":{{"Ref":"{SUBNET_GROUP_LOGICAL_ID}"}}"#
    ));
    template.push_str(&id_list("SecurityGroupIds", security_groups));
    template.push_str(group_properties);
    template.push_str(r#"}}},"Outputs":{"#);
    let mut published = vec![
        format!(r#""GroupRef":{{"Value":{{"Ref":"{GROUP_LOGICAL_ID}"}}}}"#),
        format!(r#""SubnetGroupRef":{{"Value":{{"Ref":"{SUBNET_GROUP_LOGICAL_ID}"}}}}"#),
    ];
    for (output, attribute) in outputs {
        published.push(format!(
            r#""{output}":{{"Value":{{"Fn::GetAtt":["{GROUP_LOGICAL_ID}","{attribute}"]}}}}"#
        ));
    }
    template.push_str(&published.join(","));
    template.push_str("}}");
    template
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events: the
/// reasons the owning service's refusal is quoted in.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack `stack_name` from `template`, with the rollback a failed resource unwinds
/// through, and answer its settled `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            ("OnFailure", "ROLLBACK"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// `DescribeReplicationGroups` for one identifier, as the status and body a client reads.
async fn describe_group(router: &MultiServiceRouter, group_id: &str) -> (u16, String) {
    elasticache(
        router,
        "DescribeReplicationGroups",
        &[("ReplicationGroupId", group_id)],
    )
    .await
}

/// The value Amazon ElastiCache's own answer carries for the type's return value `attribute`.
///
/// The map from each published attribute to the member of `ReplicationGroup` that holds it:
/// `ConfigurationEndPoint.*` is the group's `ConfigurationEndpoint`, `PrimaryEndPoint.*` and
/// `ReaderEndPoint.*` are the node group's `PrimaryEndpoint` / `ReaderEndpoint`, and
/// `ReadEndPoint.Addresses` / `.Ports` are the `ReadEndpoint` of every node group member, as
/// the delimited lists those two return values are.
fn published(reported: &str, attribute: &str) -> Option<String> {
    let (endpoint, member) = attribute.split_once('.')?;
    if endpoint == "ReadEndPoint" {
        let value = if member == "Addresses" { "Address" } else { "Port" };
        let each: Vec<String> = blocks_of(reported, "ReadEndpoint")
            .into_iter()
            .filter_map(|block| opt_element(block, value))
            .collect();
        return (!each.is_empty()).then(|| each.join(","));
    }
    // The type spells the endpoint's noun with a capital `P` where the API spells it with a
    // small one, and publishes the replicas' endpoint under the API's own `ReaderEndpoint`.
    let held = match endpoint {
        "ConfigurationEndPoint" => "ConfigurationEndpoint",
        "PrimaryEndPoint" => "PrimaryEndpoint",
        "ReaderEndPoint" => "ReaderEndpoint",
        other => other,
    };
    opt_element(block_of(reported, held)?, member)
}

/// One ACCEPTED row: a stack whose resources are a cache subnet group and a replication group
/// placed in it.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The `ReplicationGroupId` the template states, or `""` for a template that names the
    /// group nowhere — CloudFormation names it then, and the `Ref` is that name.
    group_id: &'static str,
    /// Further properties the row declares, as template JSON members (each with its leading
    /// comma), and what ElastiCache must then report for them.
    group_properties: &'static str,
    reported: &'static [(&'static str, &'static str)],
    /// The outputs the row publishes: each an output name and the type return value it reads.
    endpoints: &'static [(&'static str, &'static str)],
    /// How many entries the group's read-endpoint lists must carry, which is one per node group
    /// member a cluster mode disabled group has — `0` for a cluster mode enabled group, which
    /// publishes no per-node read endpoint at all.
    read_endpoints: usize,
}

/// The endpoints a cluster mode DISABLED group publishes: the shard's primary and reader, and
/// the per-node read endpoints.
const UNPARTITIONED_ENDPOINTS: &[(&str, &str)] = &[
    ("PrimaryAddress", "PrimaryEndPoint.Address"),
    ("PrimaryPort", "PrimaryEndPoint.Port"),
    ("ReaderAddress", "ReaderEndPoint.Address"),
    ("ReaderPort", "ReaderEndPoint.Port"),
    ("ReadAddresses", "ReadEndPoint.Addresses"),
    ("ReadPorts", "ReadEndPoint.Ports"),
];

/// The endpoint a cluster mode ENABLED group publishes: "use the configuration endpoint to
/// connect to this replication group".
const PARTITIONED_ENDPOINTS: &[(&str, &str)] = &[
    ("ConfigurationAddress", "ConfigurationEndPoint.Address"),
    ("ConfigurationPort", "ConfigurationEndPoint.Port"),
];

/// The accepted rows: the axis is what the template states about the group's topology and
/// encryption, so the rule is not keyed to one declaration shape. The auth token is forty
/// printable ASCII characters, the form `CreateReplicationGroup$AuthToken` takes ("must be
/// only printable ASCII characters", "must be at least 16 characters and no more than 128
/// characters in length").
const ROWS: &[Row] = &[
    Row {
        case: "a named single-node group with in-transit encryption and an auth token",
        stack_name: "ecrg-named-encrypted",
        group_id: "cfn-ecrg-encrypted",
        group_properties: concat!(
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":1"#,
            r#","AutomaticFailoverEnabled":false,"SnapshotRetentionLimit":0"#,
            r#","TransitEncryptionEnabled":true"#,
            r#","AuthToken":"Zq7Rt2Wv9Yx4Bn6Mk8Jh3Gf5Dc1Sa0PlOiUyTrEw""#,
            r#","Tags":[{"Key":"team","Value":"platform"}]"#
        ),
        reported: &[
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("TransitEncryptionEnabled", "true"),
            ("AuthTokenEnabled", "true"),
            ("ClusterEnabled", "false"),
            ("SnapshotRetentionLimit", "0"),
        ],
        endpoints: UNPARTITIONED_ENDPOINTS,
        read_endpoints: 1,
    },
    Row {
        case: "a group CloudFormation names, with a replica and automatic failover",
        stack_name: "ecrg-generated-replica",
        group_id: "",
        group_properties: concat!(
            r#","Engine":"valkey","CacheNodeType":"cache.t4g.small","NumCacheClusters":2"#,
            r#","AutomaticFailoverEnabled":true"#,
            r#","PreferredMaintenanceWindow":"sun:23:00-mon:01:30""#
        ),
        reported: &[
            ("Engine", "valkey"),
            ("CacheNodeType", "cache.t4g.small"),
            ("AutomaticFailover", "enabled"),
            ("ClusterEnabled", "false"),
        ],
        endpoints: UNPARTITIONED_ENDPOINTS,
        read_endpoints: 2,
    },
    Row {
        case: "a group whose keyspace is partitioned across two shards",
        stack_name: "ecrg-partitioned",
        group_id: "cfn-ecrg-sharded",
        group_properties: concat!(
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumNodeGroups":2"#,
            r#","ReplicasPerNodeGroup":1,"SnapshotRetentionLimit":2"#
        ),
        reported: &[
            ("Engine", "redis"),
            ("ClusterEnabled", "true"),
            ("SnapshotRetentionLimit", "2"),
        ],
        endpoints: PARTITIONED_ENDPOINTS,
        read_endpoints: 0,
    },
];

#[tokio::test]
async fn a_completed_stack_has_created_its_replication_group_in_elasticache() {
    let router = build_router();
    let network = network(&router, &["us-east-1a", "us-east-1b"]).await;
    let security_groups = [network.security_group.as_str()];

    for row in ROWS {
        let case = row.case;
        let template = template_of(
            row.group_id,
            row.group_properties,
            &network,
            &security_groups,
            row.endpoints,
        );
        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body} with the failure reasons \
             {:?}",
            failure_reasons(&router, row.stack_name).await
        );

        // The handle the stack publishes for the group: "Ref returns the replication group's
        // id", which is the identifier every ElastiCache call addresses it by.
        let outputs = outputs_of(&body);
        let group_ref = outputs
            .get("GroupRef")
            .unwrap_or_else(|| panic!("case {case:?}: no GroupRef output in {body}"))
            .clone();
        if !row.group_id.is_empty() {
            assert_eq!(
                group_ref, row.group_id,
                "case {case:?}: expected the declared ReplicationGroupId as the Ref, got \
                 {group_ref:?}"
            );
        }

        // THE RULE, first half: the group the stack claims to have created is one ElastiCache
        // holds, under exactly that identifier, with the configuration the template declared.
        let (status, reported) = describe_group(&router, &group_ref).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ElastiCache to report the group the stack created, got \
             {status} with body {reported}"
        );
        assert_eq!(
            opt_element(&reported, "ReplicationGroupId").as_deref(),
            Some(group_ref.to_ascii_lowercase().as_str()),
            "case {case:?}: expected the group to be held under the published handle, got \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "Status").as_deref(),
            Some("available"),
            "case {case:?}: expected a completed stack to leave the group available, got \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "Description").as_deref(),
            Some(GROUP_DESCRIPTION),
            "case {case:?}: expected the declared description, got {reported}"
        );
        for (member, value) in row.reported {
            assert_eq!(
                opt_element(&reported, member).as_deref(),
                Some(*value),
                "case {case:?}: expected {member} to be {value:?}, got {reported}"
            );
        }

        // The group sits in the cache subnet group the SAME stack created, which is what makes
        // the `Ref` the template placed it through a name ElastiCache could resolve.
        let subnet_group_ref = outputs
            .get("SubnetGroupRef")
            .unwrap_or_else(|| panic!("case {case:?}: no SubnetGroupRef output in {body}"))
            .clone();
        let (status, groups) = elasticache(
            &router,
            "DescribeCacheSubnetGroups",
            &[("CacheSubnetGroupName", subnet_group_ref.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ElastiCache to hold the subnet group the stack created, \
             got {status} with body {groups}"
        );
        for subnet in &network.subnets {
            assert!(
                elements(&groups, "SubnetIdentifier")
                    .iter()
                    .any(|held| held == subnet),
                "case {case:?}: expected the subnet group to gather {subnet:?}, got {groups}"
            );
        }

        // THE RULE, second half: every endpoint the stack published is a value ElastiCache
        // reports for the group — present, and that service's own.
        for (output, attribute) in row.endpoints {
            let value = outputs.get(*output).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the {output:?} output over {attribute:?} carries no value in \
                     {body}"
                )
            });
            assert_eq!(
                Some(value.clone()),
                published(&reported, attribute),
                "case {case:?}: expected the published {attribute:?} to be ElastiCache's own, \
                 got {reported}"
            );
        }
        let read_endpoints = outputs
            .get("ReadAddresses")
            .map(|addresses| addresses.split(',').count())
            .unwrap_or_default();
        assert_eq!(
            read_endpoints,
            row.read_endpoints,
            "case {case:?}: expected {} read endpoint(s) published, got {:?}",
            row.read_endpoints,
            outputs.get("ReadAddresses")
        );

        // And the group the stack created takes a live call, which is what makes it the
        // account's group rather than a record CloudFormation holds alone.
        let (status, modified) = elasticache(
            &router,
            "ModifyReplicationGroup",
            &[
                ("ReplicationGroupId", group_ref.as_str()),
                ("ApplyImmediately", "true"),
                ("ReplicationGroupDescription", "modified through the service"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the stack's group to take a modify, got {status} with body \
             {modified}"
        );
        assert_eq!(
            opt_element(&modified, "Description").as_deref(),
            Some("modified through the service"),
            "case {case:?}: expected the modify to be applied, got {modified}"
        );
    }
}

/// The properties a group of one node is declared with, at the snapshot retention the update
/// test changes.
fn single_node_properties(retention: &str) -> String {
    format!(
        r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":1,"AutomaticFailoverEnabled":false,"SnapshotRetentionLimit":{retention}"#
    )
}

#[tokio::test]
async fn a_completed_stack_has_named_the_snapshot_source_its_template_declared() {
    let case = "a group whose template names its own member cluster as the snapshot source";
    let stack_name = "ecrg-snapshot-source";
    let group_id = "cfn-ecrg-snapshotting";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;
    let security_groups = [network.security_group.as_str()];

    // "A Valkey or Redis OSS (cluster mode disabled) replication group numbers its member
    // clusters `<group>-001`, `<group>-002`, …", so the primary of this group is the source the
    // template names — a cluster that does not exist until the group does, which is why the
    // property reaches ElastiCache through the modify rather than the create.
    let source = format!("{group_id}-001");
    let properties = format!(
        r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":2,"AutomaticFailoverEnabled":true,"SnapshotRetentionLimit":3,"SnapshottingClusterId":"{source}""#
    );
    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(
            group_id,
            &properties,
            &network,
            &security_groups,
            UNPARTITIONED_ENDPOINTS,
        ),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );

    // THE RULE: every declared property reaches the owning service, through whichever of its
    // calls takes it — so the group ElastiCache holds names the declared snapshot source.
    let (status, reported) = describe_group(&router, group_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ElastiCache to report the group, got {status} with body \
         {reported}"
    );
    assert_eq!(
        opt_element(&reported, "SnapshottingClusterId").as_deref(),
        Some(source.as_str()),
        "case {case:?}: expected the declared snapshot source to reach the group, got {reported}"
    );
}

#[tokio::test]
async fn updating_the_stack_applies_the_change_to_the_group_elasticache_holds() {
    let case = "a changed snapshot posture reaches the group";
    let stack_name = "ecrg-updated";
    let group_id = "cfn-ecrg-updated";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;
    let security_groups = [network.security_group.as_str()];

    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(
            group_id,
            &single_node_properties("0"),
            &network,
            &security_groups,
            UNPARTITIONED_ENDPOINTS,
        ),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );

    // THE RULE on an UPDATE: a changed declaration reaches the owning service, so the group
    // ElastiCache holds carries the new configuration — the snapshot source named for the first
    // time among it, read off the group's own membership rather than guessed.
    let (status, reported) = describe_group(&router, group_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ElastiCache to report the group, got {status} with body \
         {reported}"
    );
    let member = block_of(&reported, "MemberClusters")
        .and_then(|block| opt_element(block, "ClusterId"))
        .unwrap_or_else(|| panic!("case {case:?}: no member cluster in {reported}"));
    let updated = template_of(
        group_id,
        &format!(
            "{},\"SnapshottingClusterId\":\"{member}\"",
            single_node_properties("1")
        ),
        &network,
        &security_groups,
        UNPARTITIONED_ENDPOINTS,
    );
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", stack_name), ("TemplateBody", &updated)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let body = settled_stack(&router, case, stack_name).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );

    let (status, reported) = describe_group(&router, group_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the group to still be held, got {status} with body {reported}"
    );
    assert_eq!(
        opt_element(&reported, "SnapshotRetentionLimit").as_deref(),
        Some("1"),
        "case {case:?}: expected the changed retention to reach the group, got {reported}"
    );
    assert_eq!(
        opt_element(&reported, "SnapshottingClusterId").as_deref(),
        Some(member.as_str()),
        "case {case:?}: expected the newly declared snapshot source to reach the group, got \
         {reported}"
    );
}

/// One REFUSED row: a declaration the owning service does not take, and the sentence its
/// refusal carries — which CloudFormation records as the failed resource's status reason.
struct RefusedRow {
    case: &'static str,
    stack_name: &'static str,
    group_id: &'static str,
    /// The properties the row declares beside the description and the placement.
    group_properties: &'static str,
    /// Whether the row places the group in the account's security group or in one EC2 does not
    /// hold.
    absent_security_group: bool,
    /// The fixed opening of ElastiCache's own refusal sentence for that rule.
    sentence: &'static str,
}

/// The refused rows: one per RULE of the owning service a declaration can break, so the
/// assertion is that the stack's create is that service's own rather than that one template
/// fails.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "an auth token the engine's password rules refuse",
        stack_name: "ecrg-bad-auth-token",
        group_id: "cfn-ecrg-bad-token",
        group_properties: concat!(
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":1"#,
            r#","TransitEncryptionEnabled":true,"AuthToken":"tooshort""#
        ),
        absent_security_group: false,
        sentence: "Invalid AuthToken provided.",
    },
    RefusedRow {
        case: "automatic failover on a group that holds one cache cluster",
        stack_name: "ecrg-failover-without-replica",
        group_id: "cfn-ecrg-no-replica",
        group_properties: concat!(
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":1"#,
            r#","AutomaticFailoverEnabled":true"#
        ),
        absent_security_group: false,
        sentence: "When using automatic failover, there must be at least 2 cache clusters in \
                   the replication group.",
    },
    RefusedRow {
        case: "a security group identifier Amazon EC2 holds no group for",
        stack_name: "ecrg-absent-security-group",
        group_id: "cfn-ecrg-absent-group",
        group_properties: r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheClusters":1"#,
        absent_security_group: true,
        sentence: "Some security group Id not recognized by EC2",
    },
];

#[tokio::test]
async fn a_declaration_elasticache_refuses_rolls_the_stack_back_with_no_group_left_behind() {
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;

    for row in REFUSED_ROWS {
        let case = row.case;
        let security_groups: Vec<&str> = if row.absent_security_group {
            vec![ABSENT_SECURITY_GROUP]
        } else {
            vec![network.security_group.as_str()]
        };
        let template = template_of(
            row.group_id,
            row.group_properties,
            &network,
            &security_groups,
            &[],
        );
        let body = create_and_settle(&router, case, row.stack_name, &template).await;

        // THE RULE: the create is the owning service's own, so its refusal fails the resource
        // and the stack unwinds.
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the stack to roll back, got {body} with the failure reasons \
             {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let reasons = failure_reasons(&router, row.stack_name).await;
        assert!(
            reasons.iter().any(|reason| reason.contains(row.sentence)),
            "case {case:?}: expected a failure reason quoting {:?}, got {reasons:?}",
            row.sentence
        );

        // And nothing is left behind: the group the template asked for was never created, so
        // the owning service answers its own not-found for that identifier.
        let (status, reported) = describe_group(&router, row.group_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected no group to be left behind, got {status} with body \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "Code").as_deref(),
            Some("ReplicationGroupNotFoundFault"),
            "case {case:?}: expected the modelled not-found code, got {reported}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_replication_group_it_created() {
    let case = "delete the stack that created the replication group";
    let stack_name = "ecrg-deleted";
    let group_id = "cfn-ecrg-deleted";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;
    let security_groups = [network.security_group.as_str()];

    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(
            group_id,
            &single_node_properties("0"),
            &network,
            &security_groups,
            UNPARTITIONED_ENDPOINTS,
        ),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );
    let (status, reported) = describe_group(&router, group_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the group to exist before the delete, got {status} with body \
         {reported}"
    );

    let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );

    // The group the stack created goes with the stack. ElastiCache leaves a deleted group
    // `deleting` until the removal its clock owes comes due, so the suite waits for the
    // CONDITION — the group being gone — rather than for a fixed time.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, reported) = describe_group(&router, group_id).await;
        if status == 404 {
            assert_eq!(
                opt_element(&reported, "Code").as_deref(),
                Some("ReplicationGroupNotFoundFault"),
                "case {case:?}: expected the modelled not-found code, got {reported}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to be gone with the stack, last read {status} with \
             body {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}
