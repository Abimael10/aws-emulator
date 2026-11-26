// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ElastiCache::SubnetGroup` and places a cache
//! in it has really created that group — in Amazon ElastiCache, over the account's own subnets
//! — so the cache's own create resolves the name the stack published for it, the stack
//! completes, and the endpoint outputs the template reads carry values.
//!
//! The rule under test: **a stack reaches its completed status only once the service that owns
//! each declared resource has created it, under a name that service accepts, and every
//! attribute the type publishes is read off that service's record of the resource** — so a
//! cache placed in a stack-declared subnet group is created rather than refused, and an output
//! over `Fn::GetAtt` on its endpoint carries the address and port ElastiCache published.
//! Three crates have to agree for that to hold (CloudFormation makes the calls, ElastiCache
//! serves the group and the cluster, Amazon EC2 owns the subnets and the security group), so
//! the suite lives here.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the cluster
//! or leaves CloudFormation to name it (and, where it does not, whether the generated name is
//! longer than the identifier the owning service accepts), which engine and how many nodes it
//! declares, and which of the two endpoint attribute pairs the type publishes it reads.
//!
//! Nothing here reaches into either service's state: a subnet group exists only because
//! `DescribeCacheSubnetGroups` reported it, a cluster only because `DescribeCacheClusters` did,
//! and a subnet only because EC2 minted it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The API versions each of the three wires carries beside `Action`.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
const EC2_API_VERSION: &str = "2016-11-15";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack's own clock-owed transition and the created cluster's
/// provisioning span both have to come due, and the suite waits for the CONDITION rather than
/// for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The logical ids every template below declares its resources under.
const CLUSTER_LOGICAL_ID: &str = "Cluster";
const SUBNET_GROUP_LOGICAL_ID: &str = "SubnetGroup";
const SECURITY_GROUP_LOGICAL_ID: &str = "SecurityGroup";

/// The LONGEST `CacheClusterId` Amazon ElastiCache accepts, and the characters it holds one to:
/// "A name must contain from 1 to 50 alphanumeric characters or hyphens" and "This parameter is
/// stored as a lowercase string" (`CreateCacheCluster$CacheClusterId`). A name CloudFormation
/// GENERATES for a cluster has to be inside both, or it names a cluster that service could
/// never hold.
const CACHE_CLUSTER_ID_MAX_CHARACTERS: usize = 50;

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

/// The account's VPC and its subnets, built through Amazon EC2's own operations, so every
/// subnet a template below hands ElastiCache is one the owner of those resources handed out.
struct Network {
    vpc: String,
    subnets: Vec<String>,
}

/// Build [`Network`] with one subnet per zone.
async fn network(router: &MultiServiceRouter, zones: &[&str]) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.11.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.11.{}.0/24", index + 1);
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
    Network { vpc, subnets }
}

/// The `"SubnetIds":[…]` template member for these identifiers.
fn subnet_ids(ids: &[&str]) -> String {
    let quoted: Vec<String> = ids.iter().map(|id| format!("\"{id}\"")).collect();
    format!(r#","SubnetIds":[{}]"#, quoted.join(","))
}

/// The template one row deploys: a security group of the account's VPC, the cache subnet group
/// its subnets are gathered into, the cluster placed in both, and the endpoint outputs the row
/// reads.
///
/// This is how a template states a cache's placement: the group through `Ref` on the subnet
/// group it declares, the security group through `Fn::GetAtt` on the one it declares — so every
/// identifier ElastiCache resolves is one the stack's own resources published.
fn template_of(
    cluster_name: &str,
    cluster_properties: &str,
    subnet_group_description: &str,
    network: &Network,
    outputs: &[(&str, &str)],
) -> String {
    let subnets: Vec<&str> = network.subnets.iter().map(String::as_str).collect();
    let mut template = String::from(r#"{"Resources":{""#);
    template.push_str(SECURITY_GROUP_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::EC2::SecurityGroup","Properties":{"#);
    template.push_str(&format!(
        r#""GroupDescription":"the caches a stack declares","VpcId":"{}""#,
        network.vpc
    ));
    template.push_str(r#"}},""#);
    template.push_str(SUBNET_GROUP_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::ElastiCache::SubnetGroup","Properties":{"#);
    template.push_str(&format!(r#""Description":"{subnet_group_description}""#));
    template.push_str(&subnet_ids(&subnets));
    template.push_str(r#"}},""#);
    template.push_str(CLUSTER_LOGICAL_ID);
    template.push_str(r#"":{"Type":"AWS::ElastiCache::CacheCluster","Properties":{"#);
    if !cluster_name.is_empty() {
        template.push_str(&format!(r#""ClusterName":"{cluster_name}","#));
    }
    template.push_str(&format!(
        r#""CacheSubnetGroupName":{{"Ref":"{SUBNET_GROUP_LOGICAL_ID}"}},"#
    ));
    template.push_str(&format!(
        r#""VpcSecurityGroupIds":[{{"Fn::GetAtt":["{SECURITY_GROUP_LOGICAL_ID}","GroupId"]}}]"#
    ));
    template.push_str(cluster_properties);
    template.push_str(r#"}}},"Outputs":{"#);
    let mut published = vec![
        format!(r#""ClusterRef":{{"Value":{{"Ref":"{CLUSTER_LOGICAL_ID}"}}}}"#),
        format!(r#""SubnetGroupRef":{{"Value":{{"Ref":"{SUBNET_GROUP_LOGICAL_ID}"}}}}"#),
    ];
    for (output, attribute) in outputs {
        published.push(format!(
            r#""{output}":{{"Value":{{"Fn::GetAtt":["{CLUSTER_LOGICAL_ID}","{attribute}"]}}}}"#
        ));
    }
    template.push_str(&published.join(","));
    template.push_str("}}");
    template
}

/// Why a stack's resources failed, as CloudFormation records it on the stack's own events.
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

/// `DescribeCacheClusters` for one identifier, as the status and body a client reads.
async fn describe_cluster(router: &MultiServiceRouter, cluster_id: &str) -> (u16, String) {
    elasticache(
        router,
        "DescribeCacheClusters",
        &[("CacheClusterId", cluster_id)],
    )
    .await
}

/// `DescribeCacheSubnetGroups` for one name, as the status and body a client reads.
async fn describe_subnet_group(router: &MultiServiceRouter, name: &str) -> (u16, String) {
    elasticache(
        router,
        "DescribeCacheSubnetGroups",
        &[("CacheSubnetGroupName", name)],
    )
    .await
}

/// One row of the table: a stack whose cluster sits in the subnet group it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The `ClusterName` the template states, or `""` for a template that names the cluster
    /// nowhere — CloudFormation names it then, and the `Ref` is that name.
    cluster_name: &'static str,
    /// Further properties the row declares, as template JSON members (each with its leading
    /// comma), and what ElastiCache must then report for them.
    cluster_properties: &'static str,
    reported: &'static [(&'static str, &'static str)],
    /// The two endpoint outputs the row publishes: each an output name and the type return
    /// value it reads.
    endpoints: &'static [(&'static str, &'static str)],
}

/// The endpoint pair a Valkey or Redis OSS cluster's template reads.
const REDIS_ENDPOINT: &[(&str, &str)] = &[
    ("Address", "RedisEndpoint.Address"),
    ("Port", "RedisEndpoint.Port"),
];

/// The endpoint pair a Memcached cluster's template reads.
const CONFIGURATION_ENDPOINT: &[(&str, &str)] = &[
    ("Address", "ConfigurationEndpoint.Address"),
    ("Port", "ConfigurationEndpoint.Port"),
];

/// The rows: the axis is what the template states about the cluster and how it is named, so the
/// rule is not keyed to one declaration shape. The second row's stack and logical names are
/// long enough together that the name CloudFormation generates for the cluster does not fit
/// `CacheClusterId`'s own bound unless it is fitted to it.
const ROWS: &[Row] = &[
    Row {
        case: "a named single-node Redis OSS cluster reading its Redis endpoint",
        stack_name: "ecc-in-subnet-group-redis",
        cluster_name: "cfn-ecc-sg-redis",
        cluster_properties: concat!(
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheNodes":1"#,
            r#","Port":6379,"Tags":[{"Key":"team","Value":"platform"}]"#
        ),
        reported: &[("Engine", "redis"), ("NumCacheNodes", "1")],
        endpoints: REDIS_ENDPOINT,
    },
    Row {
        case: "a Memcached cluster of two nodes whose name CloudFormation generates",
        stack_name: "ecc-in-subnet-group-with-a-generated-cluster-name",
        cluster_name: "",
        cluster_properties: concat!(
            r#","Engine":"memcached","CacheNodeType":"cache.t3.micro","NumCacheNodes":2"#,
            r#","Port":11211"#
        ),
        reported: &[("Engine", "memcached"), ("NumCacheNodes", "2")],
        endpoints: CONFIGURATION_ENDPOINT,
    },
    Row {
        case: "a named single-node Valkey cluster with a maintenance window",
        stack_name: "ecc-in-subnet-group-valkey",
        cluster_name: "Cfn-ECC-SG-Valkey",
        cluster_properties: concat!(
            r#","Engine":"valkey","CacheNodeType":"cache.t4g.small","NumCacheNodes":1"#,
            r#","PreferredMaintenanceWindow":"sun:23:00-mon:01:30""#
        ),
        reported: &[
            ("Engine", "valkey"),
            ("PreferredMaintenanceWindow", "sun:23:00-mon:01:30"),
        ],
        endpoints: REDIS_ENDPOINT,
    },
];

/// The description every row gives the cache subnet group it declares.
const SUBNET_GROUP_DESCRIPTION: &str = "the subnets a stack gathers";

#[tokio::test]
async fn a_completed_stack_has_created_the_subnet_group_its_cache_sits_in() {
    let router = build_router();
    let network = network(&router, &["us-east-1a", "us-east-1b"]).await;

    for row in ROWS {
        let case = row.case;
        let template = template_of(
            row.cluster_name,
            row.cluster_properties,
            SUBNET_GROUP_DESCRIPTION,
            &network,
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

        let outputs = outputs_of(&body);
        let subnet_group_ref = outputs
            .get("SubnetGroupRef")
            .unwrap_or_else(|| panic!("case {case:?}: no SubnetGroupRef output in {body}"))
            .clone();
        let cluster_ref = outputs
            .get("ClusterRef")
            .unwrap_or_else(|| panic!("case {case:?}: no ClusterRef output in {body}"))
            .clone();

        // THE RULE, first half: the subnet group the stack published is one ElastiCache holds,
        // under that very name, gathering the account's own subnets.
        let (status, groups) = describe_subnet_group(&router, &subnet_group_ref).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ElastiCache to hold the subnet group the stack created, got \
             {status} with body {groups}"
        );
        assert_eq!(
            opt_element(&groups, "CacheSubnetGroupDescription").as_deref(),
            Some(SUBNET_GROUP_DESCRIPTION),
            "case {case:?}: expected the declared description, got {groups}"
        );
        for subnet in &network.subnets {
            assert!(
                elements(&groups, "SubnetIdentifier")
                    .iter()
                    .any(|held| held == subnet),
                "case {case:?}: expected the subnet group to gather {subnet:?}, got {groups}"
            );
        }

        // The name CloudFormation generates for a cluster it was not given a name for is one
        // the owning service accepts, which is why the create below was not refused.
        if row.cluster_name.is_empty() {
            assert!(
                cluster_ref.chars().count() <= CACHE_CLUSTER_ID_MAX_CHARACTERS
                    && cluster_ref
                        .chars()
                        .all(|held| held.is_ascii_lowercase()
                            || held.is_ascii_digit()
                            || held == '-'),
                "case {case:?}: expected a generated name inside CacheClusterId's own bound and \
                 character set, got {cluster_ref:?}"
            );
        }

        // THE RULE, second half: the cluster was created in the subnet group the stack
        // published, and ElastiCache reports it under the handle the stack published.
        let (status, reported) = describe_cluster(&router, &cluster_ref).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ElastiCache to report the cluster the stack created, got \
             {status} with body {reported}"
        );
        assert_eq!(
            opt_element(&reported, "CacheClusterStatus").as_deref(),
            Some("available"),
            "case {case:?}: expected a completed stack to leave the cluster available, got \
             {reported}"
        );
        assert_eq!(
            opt_element(&reported, "CacheSubnetGroupName").as_deref(),
            Some(subnet_group_ref.to_ascii_lowercase().as_str()),
            "case {case:?}: expected the cluster to sit in the stack's own subnet group, got \
             {reported}"
        );
        for (member, value) in row.reported {
            assert_eq!(
                opt_element(&reported, member).as_deref(),
                Some(*value),
                "case {case:?}: expected {member} to be {value:?}, got {reported}"
            );
        }

        // THE RULE, third half: both endpoint outputs carry a value, and it is the one
        // ElastiCache published for the cluster — the address a client connects to and the port
        // it dials.
        let endpoint = block_of(&reported, "ConfigurationEndpoint")
            .unwrap_or_else(|| panic!("case {case:?}: no endpoint in {reported}"));
        for (output, attribute) in row.endpoints {
            let value = outputs.get(*output).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the {output:?} output over {attribute:?} carries no value in \
                     {body}"
                )
            });
            let member = attribute
                .split_once('.')
                .map(|(_, member)| member)
                .unwrap_or_default();
            assert_eq!(
                Some(value.as_str()),
                opt_element(endpoint, member).as_deref(),
                "case {case:?}: expected the published {attribute:?} to be ElastiCache's own, \
                 got {endpoint}"
            );
        }
    }
}

#[tokio::test]
async fn updating_the_stack_applies_the_change_to_the_subnet_group_elasticache_holds() {
    let case = "a changed subnet group description reaches the group";
    let stack_name = "ecc-subnet-group-updated";
    let cluster_name = "cfn-ecc-sg-updated";
    let changed = "the subnets a stack gathers, described again";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;
    let properties = r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheNodes":1"#;

    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(
            cluster_name,
            properties,
            SUBNET_GROUP_DESCRIPTION,
            &network,
            REDIS_ENDPOINT,
        ),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );
    let subnet_group_ref = outputs_of(&body)
        .get("SubnetGroupRef")
        .unwrap_or_else(|| panic!("case {case:?}: no SubnetGroupRef output in {body}"))
        .clone();

    // THE RULE on an UPDATE: a changed declaration reaches the owning service, so the group
    // ElastiCache holds carries the new description.
    let updated = template_of(
        cluster_name,
        properties,
        changed,
        &network,
        REDIS_ENDPOINT,
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
    let (status, groups) = describe_subnet_group(&router, &subnet_group_ref).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the group to still be held, got {status} with body {groups}"
    );
    assert_eq!(
        opt_element(&groups, "CacheSubnetGroupDescription").as_deref(),
        Some(changed),
        "case {case:?}: expected the changed description to reach the group, got {groups}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_cache_and_then_its_subnet_group() {
    let case = "delete the stack that created the cluster and its subnet group";
    let stack_name = "ecc-subnet-group-deleted";
    let cluster_name = "cfn-ecc-sg-deleted";
    let router = build_router();
    let network = network(&router, &["us-east-1a"]).await;

    let body = create_and_settle(
        &router,
        case,
        stack_name,
        &template_of(
            cluster_name,
            r#","Engine":"redis","CacheNodeType":"cache.t3.micro","NumCacheNodes":1"#,
            SUBNET_GROUP_DESCRIPTION,
            &network,
            REDIS_ENDPOINT,
        ),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body} with the failure reasons {:?}",
        failure_reasons(&router, stack_name).await
    );
    let subnet_group_ref = outputs_of(&body)
        .get("SubnetGroupRef")
        .unwrap_or_else(|| panic!("case {case:?}: no SubnetGroupRef output in {body}"))
        .clone();

    let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );

    // Both resources go with the stack, and in that order: ElastiCache refuses to delete a
    // subnet group a cluster still sits in, so the group is gone only because the cluster was
    // taken away first. The suite waits for the CONDITION — the group being gone — rather than
    // for a fixed time.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, groups) = describe_subnet_group(&router, &subnet_group_ref).await;
        if status != 200 {
            assert_eq!(
                opt_element(&groups, "Code").as_deref(),
                Some("CacheSubnetGroupNotFoundFault"),
                "case {case:?}: expected the modelled not-found code, got {groups}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the subnet group to be gone with the stack, last read \
             {status} with body {groups}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
    let (status, reported) = describe_cluster(&router, cluster_name).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the cluster to be gone with the stack, got {status} with body \
         {reported}"
    );
    assert_eq!(
        opt_element(&reported, "Code").as_deref(),
        Some("CacheClusterNotFound"),
        "case {case:?}: expected the modelled not-found code, got {reported}"
    );
}
