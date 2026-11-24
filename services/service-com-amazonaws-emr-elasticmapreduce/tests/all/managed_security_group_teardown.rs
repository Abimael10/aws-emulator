// SPDX-License-Identifier: CC0-1.0

//! Tearing down the security groups a launch handed Amazon EMR to manage, the way a client
//! that cleans up after itself does it: revoke the ingress Amazon EMR wrote onto them, then
//! delete the groups.
//!
//! THE RULE under test: the ingress a service authorizes onto a security group its CALLER
//! owns belongs to that group, not to the workload it was written for — Amazon EMR writes its
//! rules onto the groups a launch hands it
//! (`JobFlowInstancesConfig$EmrManagedMasterSecurityGroup`, `$EmrManagedSlaveSecurityGroup`,
//! `$ServiceAccessSecurityGroup`) as the cluster comes up and takes none of them off again,
//! so after the cluster has reached a terminal state the rules are still there, they still
//! hold the groups they reference undeletable, and it is the owner's own
//! `ec2:RevokeSecurityGroupIngress` that removes them: a 200 answer reporting the rules it
//! revoked ("returns true if the request succeeds", `RevokeSecurityGroupIngressResult$Return`,
//! beside `$RevokedSecurityGroupRules`), after which the groups delete. Which is why the
//! documented teardown of a cluster's managed groups is revoke-then-delete (Amazon EMR
//! Management Guide, "Control network traffic with security groups").
//!
//! Amazon EC2 is mounted BESIDE Amazon EMR on one engine, exactly as the assembled router
//! mounts them, and every rule asserted here is read back out of the service that OWNS
//! security group rules.
//!
//! The rows span what the rule generalises over: which groups the launch handed over (both
//! node groups, both plus a service-access group, one group filling both node roles), how the
//! cluster ended (a `TerminateJobFlows` call, or a transient cluster shutting itself down),
//! and how the teardown NAMES the rules it revokes (by rule properties, or by the rule ids
//! Amazon EC2 minted for them).

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

/// The one rule the CALLER declares on the primary group of every case — a path of its own,
/// from a CIDR range rather than from another group, so it creates no dependency between the
/// groups and what survives the teardown is read off it alone.
const CALLER_PROTOCOL: &str = "tcp";
const CALLER_PORT: i32 = 22;
const CALLER_CIDR: &str = "203.0.113.0/24";

/// The grants Amazon EMR writes onto the groups it manages, written out here from the rules
/// the Amazon EMR Management Guide publishes for them ("Control network traffic with security
/// groups") rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make a case below pass: on each group the cluster's NODES run behind,
/// all ICMP, all TCP and all UDP from the cluster's node groups, plus the service's port from
/// a named service-access group; and on the service-access group itself, the nodes' port from
/// the group the primary node runs behind.
const ALL_ICMP: (&str, i32, i32) = ("icmp", -1, -1);
const ALL_TCP: (&str, i32, i32) = ("tcp", 0, 65_535);
const ALL_UDP: (&str, i32, i32) = ("udp", 0, 65_535);
const SERVICE_ACCESS_TO_NODES: (&str, i32, i32) = ("tcp", 8_443, 8_443);
const NODES_TO_SERVICE_ACCESS: (&str, i32, i32) = ("tcp", 9_443, 9_443);

/// How long a look for a clock-owed move keeps looking before giving up, and how often it
/// looks. Generous and only here so a failure is a failure rather than a hang: every case
/// waits for the CONDITION, so a loaded host delays a case instead of breaking it.
const WAIT_DEADLINE: Duration = Duration::from_secs(240);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Amazon EMR and Amazon EC2 on one engine, plus an EC2 client over the router's own
/// in-process callback — the seam the rules are read and revoked through.
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

/// A VPC and a subnet of it, both minted by the service that owns them. A VPC a caller
/// created is a NONDEFAULT VPC, which is the one a revoke of a rule the group does not hold
/// is refused in — so every rule revoked below has to be a rule that is really there.
async fn network(emulator: &Emulator) -> (String, String) {
    let vpc_id = emulator
        .ec2
        .create_vpc()
        .cidr_block("10.0.0.0/16")
        .send()
        .await
        .expect("EC2 must create the VPC")
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc reports the VPC it minted")
        .to_string();
    let subnet_id = emulator
        .ec2
        .create_subnet()
        .vpc_id(&vpc_id)
        .cidr_block("10.0.1.0/24")
        .availability_zone(ZONE)
        .send()
        .await
        .expect("EC2 must create the subnet")
        .subnet()
        .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
        .expect("CreateSubnet reports the subnet it minted")
        .to_string();
    (vpc_id, subnet_id)
}

/// One security group of `vpc_id`, as the caller that owns it creates it.
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

/// The caller's own rule on `group_id`: its path, from a CIDR range of its own.
async fn declare_caller_rule(emulator: &Emulator, group_id: &str) {
    emulator
        .ec2
        .authorize_security_group_ingress()
        .group_id(group_id)
        .ip_permissions(
            ::aws_sdk_ec2::types::IpPermission::builder()
                .ip_protocol(CALLER_PROTOCOL)
                .from_port(CALLER_PORT)
                .to_port(CALLER_PORT)
                .ip_ranges(
                    ::aws_sdk_ec2::types::IpRange::builder()
                        .cidr_ip(CALLER_CIDR)
                        .build(),
                )
                .build(),
        )
        .send()
        .await
        .expect("EC2 must authorize the caller's own rule");
}

/// One inbound grant of one group, as a comparable row: the group it is on, the protocol and
/// port range it admits, and the ONE source it admits that from — a group id or a CIDR range.
///
/// A permission is flattened to one row per source, so what is compared does not depend on how
/// Amazon EC2 groups several sources of one rule together.
fn atom(group_id: &str, protocol: &str, from_port: Option<i32>, to_port: Option<i32>, source: &str) -> String {
    let port = |value: Option<i32>| value.map_or_else(|| "none".to_string(), |port| port.to_string());
    format!("{group_id}|{protocol}|{}|{}|{source}", port(from_port), port(to_port))
}

/// Every inbound grant Amazon EC2 reports for `group_ids`, flattened by [`atom`].
async fn inbound_atoms(emulator: &Emulator, group_ids: &[String]) -> BTreeSet<String> {
    let answer = emulator
        .ec2
        .describe_security_groups()
        .set_group_ids(Some(group_ids.to_vec()))
        .send()
        .await
        .expect("EC2 must describe the groups");
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
            for range in permission.ip_ranges() {
                found.insert(atom(
                    group_id,
                    protocol,
                    permission.from_port(),
                    permission.to_port(),
                    range.cidr_ip().unwrap_or_default(),
                ));
            }
        }
    }
    found
}

/// One grant of the catalogue: the group it is written onto, its protocol and port range, and
/// the groups it admits that from.
#[derive(Debug, Clone)]
struct Grant {
    group_id: String,
    protocol: &'static str,
    from_port: i32,
    to_port: i32,
    source_group_ids: Vec<String>,
}

/// The ingress Amazon EMR writes onto the groups a launch handed it, in the form a teardown
/// revokes it in.
fn managed_grants(master: Option<&str>, slave: Option<&str>, service_access: Option<&str>) -> Vec<Grant> {
    let mut nodes: Vec<String> = Vec::new();
    for group in [master, slave].into_iter().flatten() {
        if !nodes.iter().any(|held| held == group) {
            nodes.push(group.to_string());
        }
    }
    let mut grants: Vec<Grant> = Vec::new();
    if nodes.is_empty() {
        return grants;
    }
    for group_id in &nodes {
        for (protocol, from_port, to_port) in [ALL_ICMP, ALL_TCP, ALL_UDP] {
            grants.push(Grant {
                group_id: group_id.clone(),
                protocol,
                from_port,
                to_port,
                source_group_ids: nodes.clone(),
            });
        }
        if let Some(service_access) = service_access {
            let (protocol, from_port, to_port) = SERVICE_ACCESS_TO_NODES;
            grants.push(Grant {
                group_id: group_id.clone(),
                protocol,
                from_port,
                to_port,
                source_group_ids: vec![service_access.to_string()],
            });
        }
    }
    if let (Some(service_access), Some(master)) = (service_access, master) {
        let (protocol, from_port, to_port) = NODES_TO_SERVICE_ACCESS;
        grants.push(Grant {
            group_id: service_access.to_string(),
            protocol,
            from_port,
            to_port,
            source_group_ids: vec![master.to_string()],
        });
    }
    grants
}

/// Those same grants as the rows [`inbound_atoms`] reports them in.
fn managed_atoms(grants: &[Grant]) -> BTreeSet<String> {
    let mut atoms = BTreeSet::new();
    for grant in grants {
        for source in &grant.source_group_ids {
            atoms.insert(atom(
                &grant.group_id,
                grant.protocol,
                Some(grant.from_port),
                Some(grant.to_port),
                source,
            ));
        }
    }
    atoms
}

/// One grant in the shape a revoke names it by its PROPERTIES.
fn ip_permission(grant: &Grant) -> ::aws_sdk_ec2::types::IpPermission {
    let pairs: Vec<::aws_sdk_ec2::types::UserIdGroupPair> = grant
        .source_group_ids
        .iter()
        .map(|group_id| {
            ::aws_sdk_ec2::types::UserIdGroupPair::builder()
                .group_id(group_id)
                .build()
        })
        .collect();
    ::aws_sdk_ec2::types::IpPermission::builder()
        .ip_protocol(grant.protocol)
        .from_port(grant.from_port)
        .to_port(grant.to_port)
        .set_user_id_group_pairs(Some(pairs))
        .build()
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

/// The state `cluster_id` reports.
async fn cluster_state(emulator: &Emulator, cluster_id: &str) -> String {
    emr(
        &emulator.router,
        "DescribeCluster",
        json!({ "ClusterId": cluster_id }),
    )
    .await["Cluster"]["Status"]["State"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look until `cluster_id` has reached a terminal state, which for a transient cluster is a
/// move the clock owes rather than one any call makes.
async fn settled_cluster(emulator: &Emulator, case: &str, cluster_id: &str) -> String {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let state = cluster_state(emulator, cluster_id).await;
        if state.starts_with("TERMINATED") {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster must reach a terminal state, got {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ids Amazon EC2 minted for the inbound rules of `group_id` that admit another group —
/// which is every rule Amazon EMR wrote and none of the caller's own, since the caller's names
/// a CIDR range.
async fn referenced_group_rule_ids(emulator: &Emulator, group_id: &str) -> Vec<String> {
    let answer = emulator
        .ec2
        .describe_security_group_rules()
        .filters(
            ::aws_sdk_ec2::types::Filter::builder()
                .name("group-id")
                .values(group_id)
                .build(),
        )
        .send()
        .await
        .expect("EC2 must describe the group's rules");
    answer
        .security_group_rules()
        .iter()
        .filter(|rule| !rule.is_egress().unwrap_or(false))
        .filter(|rule| {
            rule.referenced_group_info()
                .and_then(::aws_sdk_ec2::types::ReferencedSecurityGroup::group_id)
                .is_some()
        })
        .filter_map(|rule| rule.security_group_rule_id().map(str::to_string))
        .collect()
}

/// How a teardown names the rules it revokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedBy {
    /// "You can specify rules using either rule IDs or security group rule properties"
    /// (`RevokeSecurityGroupIngress`) — these are the properties.
    Properties,
    /// And these are the ids Amazon EC2 minted.
    RuleIds,
}

/// One row of the table.
struct Case {
    label: &'static str,
    /// Whether the launch names the primary group, the core/task group and a service-access
    /// group, in that order.
    names: (bool, bool, bool),
    /// Whether ONE group fills both node roles.
    one_group_for_both_roles: bool,
    /// Whether the cluster is shut down by a `TerminateJobFlows` call; otherwise it is
    /// transient and shuts itself down.
    terminated_by_a_call: bool,
    named_by: NamedBy,
}

/// THE RULE: Amazon EMR's managed ingress outlives the cluster it was written for, and the
/// owner's own revoke is what removes it — after which the groups delete.
#[tokio::test]
async fn the_ingress_emr_wrote_outlives_the_cluster_and_the_owner_revokes_it() {
    let cases = [
        Case {
            label: "both node groups, a cluster shut down by a call, revoked by properties",
            names: (true, true, false),
            one_group_for_both_roles: false,
            terminated_by_a_call: true,
            named_by: NamedBy::Properties,
        },
        Case {
            label: "the node groups and a service-access group, revoked by properties",
            names: (true, true, true),
            one_group_for_both_roles: false,
            terminated_by_a_call: true,
            named_by: NamedBy::Properties,
        },
        Case {
            label: "one group for both node roles, a transient cluster, revoked by properties",
            names: (true, true, false),
            one_group_for_both_roles: true,
            terminated_by_a_call: false,
            named_by: NamedBy::Properties,
        },
        Case {
            label: "both node groups, a transient cluster, revoked by rule id",
            names: (true, true, false),
            one_group_for_both_roles: false,
            terminated_by_a_call: false,
            named_by: NamedBy::RuleIds,
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
        declare_caller_rule(&emulator, &primary).await;
        let declared: BTreeSet<String> = [atom(
            &primary,
            CALLER_PROTOCOL,
            Some(CALLER_PORT),
            Some(CALLER_PORT),
            CALLER_CIDR,
        )]
        .into_iter()
        .collect();

        let (names_master, names_slave, names_service_access) = case.names;
        let mut instances = json!({});
        if names_master {
            instances["EmrManagedMasterSecurityGroup"] = json!(primary);
        }
        if names_slave {
            instances["EmrManagedSlaveSecurityGroup"] = json!(core);
        }
        if names_service_access {
            instances["ServiceAccessSecurityGroup"] = json!(service_access);
        }
        if !case.terminated_by_a_call {
            // A cluster that does not stay up once its (empty) queue has run shuts itself
            // down, so nothing calls `TerminateJobFlows` for it.
            instances["KeepJobFlowAliveWhenNoSteps"] = json!(false);
        }
        let cluster_id = launch(&emulator, label, &subnet_id, instances).await;

        let grants = managed_grants(
            names_master.then_some(primary.as_str()),
            names_slave.then_some(core.as_str()),
            names_service_access.then_some(service_access.as_str()),
        );
        let mut expected = managed_atoms(&grants);
        expected.extend(declared.iter().cloned());
        // One group named for both node roles is ONE group: the list of what this case holds
        // carries it once, so nothing below reads or revokes it twice.
        let mut groups: Vec<String> = Vec::new();
        for group_id in [&primary, &core, &service_access] {
            if !groups.iter().any(|held| held == group_id) {
                groups.push(group_id.clone());
            }
        }

        if case.terminated_by_a_call {
            emr(
                &emulator.router,
                "TerminateJobFlows",
                json!({ "JobFlowIds": [cluster_id.clone()] }),
            )
            .await;
        }
        let state = settled_cluster(&emulator, label, &cluster_id).await;

        // The rules are STILL there, every one of them, once the cluster that occasioned them
        // is gone: nothing but their owner takes them off.
        let after = inbound_atoms(&emulator, &groups).await;
        assert_eq!(
            after, expected,
            "case {label:?}: a cluster that is {state:?} must leave the ingress Amazon EMR \
             wrote on the groups it was handed; missing {:?}, unexpected {:?}",
            expected.difference(&after).collect::<Vec<_>>(),
            after.difference(&expected).collect::<Vec<_>>(),
        );

        // And while they are there they are a live dependency of the groups they reference,
        // which is what makes the revoke the first step of the teardown rather than an
        // optional one. A group only its OWN rules reference is not held by them.
        for group_id in &groups {
            let held_by_another = grants
                .iter()
                .any(|grant| grant.group_id != *group_id && grant.source_group_ids.contains(group_id));
            if !held_by_another {
                continue;
            }
            let answer = emulator
                .ec2
                .delete_security_group()
                .group_id(group_id)
                .send()
                .await;
            let code = answer
                .as_ref()
                .err()
                .and_then(|error| error.as_service_error())
                .and_then(::aws_sdk_ec2::error::ProvideErrorMetadata::code)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                code, "DependencyViolation",
                "case {label:?}: {group_id} is referenced by a rule Amazon EMR wrote, so its \
                 delete must be refused, got {answer:?}"
            );
        }

        // The teardown itself: the owner revokes what Amazon EMR wrote, one call per group,
        // and every call is served — "returns true if the request succeeds"
        // (`RevokeSecurityGroupIngressResult$Return`) — reporting the rules it removed.
        for group_id in &groups {
            let on_this_group: Vec<Grant> = grants
                .iter()
                .filter(|grant| grant.group_id == *group_id)
                .cloned()
                .collect();
            if on_this_group.is_empty() {
                continue;
            }
            let wanted: usize = on_this_group
                .iter()
                .map(|grant| grant.source_group_ids.len())
                .sum();
            let revoke = emulator.ec2.revoke_security_group_ingress().group_id(group_id);
            let revoke = match case.named_by {
                NamedBy::Properties => revoke.set_ip_permissions(Some(
                    on_this_group.iter().map(ip_permission).collect(),
                )),
                NamedBy::RuleIds => revoke.set_security_group_rule_ids(Some(
                    referenced_group_rule_ids(&emulator, group_id).await,
                )),
            };
            let answered = revoke.send().await;
            let answered = answered.unwrap_or_else(|error| {
                panic!(
                    "case {label:?}: the owner's revoke of the rules Amazon EMR wrote onto \
                     {group_id} must be served, got {error:?}"
                )
            });
            assert_eq!(
                answered.r#return(),
                Some(true),
                "case {label:?}: the revoke of {group_id} must report success, got {answered:?}"
            );
            assert!(
                answered.unknown_ip_permissions().is_empty(),
                "case {label:?}: every rule the revoke of {group_id} named was there, so none \
                 is unknown, got {answered:?}"
            );
            assert_eq!(
                answered.revoked_security_group_rules().len(),
                wanted,
                "case {label:?}: the revoke of {group_id} must report the {wanted} rules it \
                 removed, got {answered:?}"
            );
        }

        // What is left is exactly what the caller declared: the revoke removed the rules it
        // named and nothing else.
        let remaining = inbound_atoms(&emulator, &groups).await;
        assert_eq!(
            remaining, declared,
            "case {label:?}: only the caller's own rule may be left after the teardown; \
             unexpected {:?}, missing {:?}",
            remaining.difference(&declared).collect::<Vec<_>>(),
            declared.difference(&remaining).collect::<Vec<_>>(),
        );

        // So the teardown converges: with nothing referencing them, the groups delete.
        for group_id in &groups {
            let deleted = emulator
                .ec2
                .delete_security_group()
                .group_id(group_id)
                .send()
                .await;
            assert!(
                deleted.is_ok(),
                "case {label:?}: once the rules are revoked {group_id} must delete, got \
                 {deleted:?}"
            );
        }
    }
}
