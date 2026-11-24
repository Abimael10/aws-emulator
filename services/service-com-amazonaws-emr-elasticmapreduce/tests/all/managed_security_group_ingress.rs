// SPDX-License-Identifier: CC0-1.0

//! The ingress Amazon EMR authorizes onto the security groups a launch hands it to manage,
//! end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, and reads the rules back out of Amazon EC2 with `DescribeSecurityGroups`.
//! Nothing reaches into either service's state struct: what is asserted is what the service
//! that OWNS security group rules reports, which is also what makes those rules a live
//! dependency of the groups.
//!
//! THE RULE: a launch that names the security groups Amazon EMR is to manage
//! (`JobFlowInstancesConfig$EmrManagedMasterSecurityGroup`, `$EmrManagedSlaveSecurityGroup`,
//! `$ServiceAccessSecurityGroup`) has those groups provisioned by Amazon EMR as the cluster
//! comes up: beside whatever the caller declared on them, each group the cluster's nodes run
//! behind admits all ICMP, all TCP and all UDP from the cluster's node groups, a named
//! service-access group is admitted to the nodes on the service's port and admits the primary
//! group on the nodes' port, and the groups a launch did NOT hand over are not written to at
//! all. The groups are the CALLER's, so the rules stay on them when the cluster shuts down,
//! and while they are there the groups they reference cannot be deleted — removing them is
//! the owner's own revoke, which `all/managed_security_group_teardown.rs` covers.
//!
//! Amazon EC2 is mounted BESIDE Amazon EMR on one engine, exactly as the assembled router
//! mounts them.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The zone the subnet every cluster here is placed in lives in.
const ZONE: &str = "us-west-2a";

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The release every launch here names.
const RELEASE_LABEL: &str = "emr-7.9.0";

/// How long a wait for a clock-owed change looks for it before giving up.
///
/// Deliberately far above what the condition needs — a transient cluster's whole lifecycle is
/// a handful of seconds — so a host running every test of the workspace at once, where a call
/// that normally answers at once takes tens of seconds, still reaches it. The case ends as
/// soon as the condition holds, so the generous bound costs nothing when it does.
const WAIT_DEADLINE: Duration = Duration::from_secs(240);

/// One look inside that deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Amazon EMR and Amazon EC2 on one engine, plus an EC2 client over the router's own
/// in-process callback — the seam this suite reads the rules back through.
struct Emulator {
    router: MultiServiceRouter,
    ec2: ::aws_sdk_ec2::Client,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
        .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Emulator {
        router,
        ec2: ::aws_sdk_ec2::Client::from_conf(config),
    }
}

/// One `awsJson1_1` Amazon EMR call, which must be served.
async fn emr(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("elasticmapreduce.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ElasticMapReduce.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261109/{REGION}/\
                 elasticmapreduce/aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// A VPC and a subnet of it, both minted by the service that owns them.
async fn network(emulator: &Emulator) -> (String, String) {
    let vpc = emulator
        .ec2
        .create_vpc()
        .cidr_block("10.0.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC");
    let vpc_id = vpc
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc reports the VPC it minted")
        .to_string();
    let created = emulator
        .ec2
        .create_subnet()
        .vpc_id(&vpc_id)
        .cidr_block("10.0.1.0/24")
        .availability_zone(ZONE)
        .send()
        .await
        .expect("EC2 must create the subnet");
    let subnet_id = created
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet reports the subnet it minted")
        .to_string();
    (vpc_id, subnet_id)
}

/// One security group of `vpc_id`, as a caller (or a CloudFormation template) creates it.
async fn security_group(emulator: &Emulator, vpc_id: &str, name: &str) -> String {
    emulator
        .ec2
        .create_security_group()
        .vpc_id(vpc_id)
        .group_name(name)
        .description(format!("{name} for an Amazon EMR cluster"))
        .send()
        .await
        .expect("EC2 must create the security group")
        .group_id()
        .expect("CreateSecurityGroup reports the group it minted")
        .to_string()
}

/// The one all-traffic rule a caller declares on `group_id` for its sibling group — the shape
/// the lifecycle's own template declares, so every rule beside it is one Amazon EMR wrote.
async fn declare_all_traffic(emulator: &Emulator, group_id: &str, source_group_id: &str) {
    emulator
        .ec2
        .authorize_security_group_ingress()
        .group_id(group_id)
        .ip_permissions(
            ::aws_sdk_ec2::types::IpPermission::builder()
                .ip_protocol("-1")
                .user_id_group_pairs(
                    ::aws_sdk_ec2::types::UserIdGroupPair::builder()
                        .group_id(source_group_id)
                        .build(),
                )
                .build(),
        )
        .send()
        .await
        .expect("EC2 must authorize the caller's own rule");
}

/// One inbound grant of one group, as a comparable row: the group it is on, the protocol and
/// port range it admits, and the ONE group it admits that from.
///
/// A permission is flattened to one row per referenced group, so what is compared does not
/// depend on how Amazon EC2 groups several sources of one rule together.
fn atom(
    group_id: &str,
    protocol: &str,
    from_port: Option<i32>,
    to_port: Option<i32>,
    source_group_id: &str,
) -> String {
    let port = |value: Option<i32>| {
        value.map_or_else(|| "none".to_string(), |port| port.to_string())
    };
    format!(
        "{group_id}|{protocol}|{}|{}|{source_group_id}",
        port(from_port),
        port(to_port)
    )
}

/// Every inbound grant Amazon EC2 reports for `group_ids`, flattened by [`atom`].
async fn inbound_atoms(emulator: &Emulator, group_ids: &[String]) -> BTreeSet<String> {
    let answer = emulator
        .ec2
        .describe_security_groups()
        .set_group_ids(Some(group_ids.to_vec()))
        .send()
        .await
        .expect("EC2 must describe the managed security groups");
    let mut found = BTreeSet::new();
    for group in answer.security_groups() {
        let group_id = group.group_id().unwrap_or_default();
        for permission in group.ip_permissions() {
            let protocol = permission.ip_protocol().unwrap_or_default();
            for pair in permission.user_id_group_pairs() {
                found.insert(atom(
                    group_id,
                    protocol,
                    permission.from_port(),
                    permission.to_port(),
                    pair.group_id().unwrap_or_default(),
                ));
            }
        }
    }
    found
}

/// The grants Amazon EMR authorizes onto the groups a launch handed it.
///
/// Written out from the rules the Amazon EMR Management Guide publishes for the groups Amazon
/// EMR manages ("Control network traffic with security groups"): on each group the cluster's
/// NODES run behind, all ICMP, all TCP and all UDP from the cluster's node groups, plus TCP
/// 8443 from a named service-access group; and on the service-access group itself, TCP 9443
/// from the group the primary node runs behind.
fn expected_managed_atoms(
    master: Option<&str>,
    slave: Option<&str>,
    service_access: Option<&str>,
) -> BTreeSet<String> {
    let mut nodes: Vec<&str> = Vec::new();
    for group in [master, slave].into_iter().flatten() {
        if !nodes.contains(&group) {
            nodes.push(group);
        }
    }
    let mut expected = BTreeSet::new();
    for group_id in &nodes {
        for (protocol, from_port, to_port) in [
            ("icmp", -1, -1),
            ("tcp", 0, 65_535),
            ("udp", 0, 65_535),
        ] {
            for source in &nodes {
                expected.insert(atom(
                    group_id,
                    protocol,
                    Some(from_port),
                    Some(to_port),
                    source,
                ));
            }
        }
        if let Some(service_access) = service_access {
            expected.insert(atom(group_id, "tcp", Some(8_443), Some(8_443), service_access));
        }
    }
    if let (Some(service_access), Some(master)) = (service_access, master) {
        expected.insert(atom(service_access, "tcp", Some(9_443), Some(9_443), master));
    }
    expected
}

/// A cluster launched in `subnet_id` with the managed groups `instances` names, answering its
/// id.
async fn launch(emulator: &Emulator, name: &str, subnet_id: &str, instances: Value) -> String {
    let mut instances = instances;
    instances["Ec2SubnetId"] = json!(subnet_id);
    instances["InstanceCount"] = json!(2);
    instances["MasterInstanceType"] = json!("m5.xlarge");
    instances["SlaveInstanceType"] = json!("m5.xlarge");
    if instances.get("KeepJobFlowAliveWhenNoSteps").is_none() {
        instances["KeepJobFlowAliveWhenNoSteps"] = json!(true);
    }
    let request = json!({
        "Name": name,
        "ReleaseLabel": RELEASE_LABEL,
        "JobFlowRole": INSTANCE_PROFILE,
        "ServiceRole": SERVICE_ROLE,
        "Instances": instances,
    });
    emr(&emulator.router, "RunJobFlow", request).await["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// The `Ec2InstanceAttributes` of `cluster_id`, as `DescribeCluster` reports them.
async fn described_attributes(emulator: &Emulator, cluster_id: &str) -> Value {
    described_cluster(emulator, cluster_id).await["Ec2InstanceAttributes"].clone()
}

/// The state `cluster_id` reports, which is what says whether its shutdown has begun.
async fn cluster_state(emulator: &Emulator, cluster_id: &str) -> String {
    described_cluster(emulator, cluster_id).await["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The cluster `DescribeCluster` reports for `cluster_id`.
async fn described_cluster(emulator: &Emulator, cluster_id: &str) -> Value {
    emr(
        &emulator.router,
        "DescribeCluster",
        json!({ "ClusterId": cluster_id }),
    )
    .await["Cluster"]
        .clone()
}

/// One row of the table: which managed groups the launch names, and which of the groups the
/// case creates carry a caller-declared all-traffic rule for their sibling.
struct Case {
    label: &'static str,
    /// Whether the launch names the primary group, the core/task group and a service-access
    /// group, in that order.
    names: (bool, bool, bool),
    /// Whether the primary and the core/task group carry the caller's own cross rule.
    declares_cross_rules: bool,
    /// Whether one group fills both node roles.
    one_group_for_both_roles: bool,
}

/// THE RULE over the ways a launch can hand Amazon EMR groups to manage.
///
/// The rows span what the rule generalises over: both node groups, both node groups plus the
/// service-access group of a private-subnet cluster, ONE group named for both node roles, and
/// a launch that hands over nothing at all — and two of them carry the caller's own rules
/// beside Amazon EMR's, so what Amazon EMR adds is read as what it adds ON TOP of those.
#[tokio::test]
async fn emr_authorizes_its_managed_ingress_onto_the_groups_a_launch_hands_it() {
    let cases = [
        Case {
            label: "the two node groups, with the caller's own cross rules",
            names: (true, true, false),
            declares_cross_rules: true,
            one_group_for_both_roles: false,
        },
        Case {
            label: "the two node groups, with nothing declared on them",
            names: (true, true, false),
            declares_cross_rules: false,
            one_group_for_both_roles: false,
        },
        Case {
            label: "the two node groups and a service-access group",
            names: (true, true, true),
            declares_cross_rules: true,
            one_group_for_both_roles: false,
        },
        Case {
            label: "one group named for both node roles",
            names: (true, true, false),
            declares_cross_rules: false,
            one_group_for_both_roles: true,
        },
        Case {
            label: "a launch that hands over no managed group",
            names: (false, false, false),
            declares_cross_rules: true,
            one_group_for_both_roles: false,
        },
    ];

    for case in cases {
        let label = case.label;
        let emulator = emulator();
        let (vpc_id, subnet_id) = network(&emulator).await;
        let primary = security_group(&emulator, &vpc_id, "emr-primary").await;
        let core = if case.one_group_for_both_roles {
            primary.clone()
        } else {
            security_group(&emulator, &vpc_id, "emr-core").await
        };
        let service_access = security_group(&emulator, &vpc_id, "emr-service-access").await;

        let mut declared = BTreeSet::new();
        if case.declares_cross_rules && primary != core {
            declare_all_traffic(&emulator, &primary, &core).await;
            declare_all_traffic(&emulator, &core, &primary).await;
            declared.insert(atom(&primary, "-1", None, None, &core));
            declared.insert(atom(&core, "-1", None, None, &primary));
        }

        let mut instances = json!({});
        let (names_master, names_slave, names_service_access) = case.names;
        if names_master {
            instances["EmrManagedMasterSecurityGroup"] = json!(primary);
        }
        if names_slave {
            instances["EmrManagedSlaveSecurityGroup"] = json!(core);
        }
        if names_service_access {
            instances["ServiceAccessSecurityGroup"] = json!(service_access);
        }
        let cluster_id = launch(&emulator, label, &subnet_id, instances).await;

        // The cluster reports back exactly the groups the launch handed it, which is what the
        // rules below are written onto.
        let attributes = described_attributes(&emulator, &cluster_id).await;
        for (member, named, group_id) in [
            ("EmrManagedMasterSecurityGroup", names_master, &primary),
            ("EmrManagedSlaveSecurityGroup", names_slave, &core),
            (
                "ServiceAccessSecurityGroup",
                names_service_access,
                &service_access,
            ),
        ] {
            let reported = attributes[member].as_str();
            let expected = named.then_some(group_id.as_str());
            assert_eq!(
                reported, expected,
                "case {label:?}: DescribeCluster must report {member} as {expected:?}, got \
                 {reported:?}"
            );
        }

        let mut expected = expected_managed_atoms(
            names_master.then_some(primary.as_str()),
            names_slave.then_some(core.as_str()),
            names_service_access.then_some(service_access.as_str()),
        );
        expected.extend(declared.iter().cloned());
        let groups = vec![primary.clone(), core.clone(), service_access.clone()];
        let found = inbound_atoms(&emulator, &groups).await;
        assert_eq!(
            found, expected,
            "case {label:?}: the inbound rules of the groups must be the caller's own plus \
             the ones Amazon EMR authorizes; missing {:?}, unexpected {:?}",
            expected.difference(&found).collect::<Vec<_>>(),
            found.difference(&expected).collect::<Vec<_>>(),
        );
    }
}

/// A cluster that is shut down by a call leaves every rule Amazon EMR wrote where it is: the
/// groups belong to the caller, so nothing but the caller's own revoke takes them off.
#[tokio::test]
async fn emr_leaves_its_ingress_in_place_when_the_cluster_is_terminated() {
    let emulator = emulator();
    let (vpc_id, subnet_id) = network(&emulator).await;
    let primary = security_group(&emulator, &vpc_id, "emr-primary").await;
    let core = security_group(&emulator, &vpc_id, "emr-core").await;
    declare_all_traffic(&emulator, &primary, &core).await;
    declare_all_traffic(&emulator, &core, &primary).await;
    let declared: BTreeSet<String> = [
        atom(&primary, "-1", None, None, &core),
        atom(&core, "-1", None, None, &primary),
    ]
    .into_iter()
    .collect();

    let cluster_id = launch(
        &emulator,
        "terminated by a call",
        &subnet_id,
        json!({
            "EmrManagedMasterSecurityGroup": primary,
            "EmrManagedSlaveSecurityGroup": core,
        }),
    )
    .await;
    let groups = vec![primary.clone(), core.clone()];
    let while_running = inbound_atoms(&emulator, &groups).await;
    assert!(
        while_running.len() > declared.len(),
        "a launched cluster's managed groups must carry rules beside the caller's own, got \
         {while_running:?}"
    );

    emr(
        &emulator.router,
        "TerminateJobFlows",
        json!({ "JobFlowIds": [cluster_id] }),
    )
    .await;
    let after = inbound_atoms(&emulator, &groups).await;
    assert_eq!(
        after, while_running,
        "the shutdown must leave the groups' rules exactly as they were; unexpected {:?}, \
         missing {:?}",
        after.difference(&while_running).collect::<Vec<_>>(),
        while_running.difference(&after).collect::<Vec<_>>(),
    );
    assert!(
        declared.is_subset(&after),
        "and the caller's own rules are among them, got {after:?}"
    );
}

/// And a cluster that shuts ITSELF down — the clock's own shutdown, which no call asked for —
/// leaves them just the same.
#[tokio::test]
async fn emr_leaves_its_ingress_in_place_when_the_cluster_terminates_itself() {
    let emulator = emulator();
    let (vpc_id, subnet_id) = network(&emulator).await;
    let primary = security_group(&emulator, &vpc_id, "emr-primary").await;
    let core = security_group(&emulator, &vpc_id, "emr-core").await;
    let cluster_id = launch(
        &emulator,
        "a transient cluster",
        &subnet_id,
        json!({
            "EmrManagedMasterSecurityGroup": primary,
            "EmrManagedSlaveSecurityGroup": core,
            // A cluster that does not stay up once its (empty) queue has run shuts itself
            // down, so nothing calls TerminateJobFlows for it.
            "KeepJobFlowAliveWhenNoSteps": false,
        }),
    )
    .await;
    let groups = vec![primary.clone(), core.clone()];
    // The rules are written as the cluster comes up, so they are there from the launch on,
    // whatever the clock has done with the cluster since.
    let while_up = inbound_atoms(&emulator, &groups).await;
    assert!(
        !while_up.is_empty(),
        "the launch must have carried Amazon EMR's rules onto its managed groups"
    );

    // Looked for until the deadline passes rather than waited out: the shutdown is the
    // clock's. The rules do NOT go with it.
    let deadline = Instant::now() + WAIT_DEADLINE;
    let state = loop {
        let state = cluster_state(&emulator, &cluster_id).await;
        if state == "TERMINATED" || Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        state, "TERMINATED",
        "the transient cluster must shut itself down, got {state:?}"
    );
    let remaining = inbound_atoms(&emulator, &groups).await;
    assert_eq!(
        remaining, while_up,
        "a cluster that shut itself down must leave the groups' rules exactly as they were; \
         unexpected {:?}, missing {:?}",
        remaining.difference(&while_up).collect::<Vec<_>>(),
        while_up.difference(&remaining).collect::<Vec<_>>(),
    );
}

/// The rules Amazon EMR adds are a live dependency of the groups they reference: a group is
/// undeletable while they are there, and they are still there once the cluster has shut down,
/// so the delete stays refused until their owner revokes them.
#[tokio::test]
async fn the_managed_groups_cannot_be_deleted_while_emrs_rules_reference_them() {
    let emulator = emulator();
    let (vpc_id, subnet_id) = network(&emulator).await;
    let primary = security_group(&emulator, &vpc_id, "emr-primary").await;
    let core = security_group(&emulator, &vpc_id, "emr-core").await;
    let cluster_id = launch(
        &emulator,
        "deleted after teardown",
        &subnet_id,
        json!({
            "EmrManagedMasterSecurityGroup": primary,
            "EmrManagedSlaveSecurityGroup": core,
        }),
    )
    .await;

    for group_id in [&primary, &core] {
        let refused = emulator
            .ec2
            .delete_security_group()
            .group_id(group_id)
            .send()
            .await
            .expect_err("a group Amazon EMR's rules reference must not delete");
        let code = refused
            .as_service_error()
            .and_then(::aws_sdk_ec2::error::ProvideErrorMetadata::code)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            code, "DependencyViolation",
            "case {group_id:?}: the delete must be refused for the dependency Amazon EMR's \
             rules create, got {code:?}"
        );
    }

    emr(
        &emulator.router,
        "TerminateJobFlows",
        json!({ "JobFlowIds": [cluster_id] }),
    )
    .await;
    // The shutdown took nothing off the groups, so the dependency is still there: the delete
    // is refused the same way until the groups' OWNER revokes the rules, which is what
    // `all/managed_security_group_teardown.rs` drives end to end.
    for group_id in [&primary, &core] {
        let refused = emulator
            .ec2
            .delete_security_group()
            .group_id(group_id)
            .send()
            .await
            .expect_err("a shut-down cluster's rules still hold the groups they reference");
        let code = refused
            .as_service_error()
            .and_then(::aws_sdk_ec2::error::ProvideErrorMetadata::code)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            code, "DependencyViolation",
            "case {group_id:?}: the delete must still be refused after the shutdown, got \
             {code:?}"
        );
    }
}
