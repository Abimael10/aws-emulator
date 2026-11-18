// SPDX-License-Identifier: CC0-1.0

//! Every security group an Amazon EKS cluster's `resourcesVpcConfig` names has to belong
//! to the VPC the cluster's network interfaces are created in.
//!
//! The RULE: `securityGroupIds` names the groups applied to "the cross-account elastic
//! network interfaces that Amazon EKS creates" in the cluster's subnets
//! (`VpcConfigRequest$securityGroupIds`), and a security group belongs to exactly one VPC
//! (`SecurityGroup$VpcId`), governing only interfaces of that VPC. So a group of another
//! VPC than the one those subnets sit in could never apply to the cluster, and the call is
//! refused 400 `InvalidParameterException` naming that group and the cluster's VPC —
//! before any cluster exists and before any update starts. It holds on both operations
//! that carry the member: `CreateCluster`, where the VPC is the one the subnets resolve
//! to, and `UpdateClusterConfig`, where it is the VPC the cluster already reports ("you
//! can't change which VPC the subnets are from").
//!
//! The rule needs EKS and EC2 on one engine, which is why this suite lives here rather
//! than in the EKS crate: a security group is EC2's resource, so EC2 is the only thing
//! that can say which VPC one is in, and an emulator with no EC2 has nothing to ask.
//!
//! The table spans what the rule generalises over: how many groups a request names (none,
//! one, the five the member allows), WHICH VPC each group is in (two the client created
//! and the account's own default one), which VPC the cluster itself is placed in, whether
//! the offending group was created by this test or is one the account simply has, and
//! where the offender sits in the list. Each refused create is then shown to leave no
//! cluster, and each served one to report the groups it was created with.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and
//! the requests driven — on a sized thread.
//!
//! The request harness is the test crate's own: `eks_wire` for the drivers and the answer,
//! `query_wire` for the XML readers.

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::eks_wire::{ec2, eks, role_arn, REGION};
use crate::query_wire::{element, elements};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How long one look at a cluster's status waits before looking again.
const POLL_WAIT: Duration = Duration::from_millis(100);

/// How many times a poll looks before it gives up — a deadline far above the span the
/// service owes a new cluster, so a loaded host still sees it reached.
const POLL_ATTEMPTS: usize = 300;

/// How many security groups the member allows: "You can specify up to five security
/// groups" (`CreateClusterRequest$resourcesVpcConfig`).
const MAX_SECURITY_GROUPS: usize = 5;

/// Which of the account's networks a row draws its subnets from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Network {
    /// The first VPC this test created.
    First,
    /// A second VPC this test created, so no row is about one VPC.
    Second,
    /// The account's own default VPC, which nothing in this test created.
    Default,
}

/// Which security group a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// One of the groups this test created in the FIRST VPC, by index.
    First(usize),
    /// The group this test created in the second VPC.
    Second,
    /// The default security group of the account's default VPC — a group nothing in this
    /// test created, which the account simply has.
    AccountDefault,
}

/// One row: the network a cluster is created in, the groups its `resourcesVpcConfig`
/// names, and the groups the refusal must name — EMPTY when Amazon EKS serves the create.
struct Row {
    case: &'static str,
    cluster: &'static str,
    network: Network,
    groups: &'static [Group],
    offenders: &'static [Group],
}

const ROWS: &[Row] = &[
    Row {
        case: "no security group at all",
        cluster: "sg-vpc-none",
        network: Network::First,
        groups: &[],
        offenders: &[],
    },
    Row {
        case: "one group of the cluster's own VPC",
        cluster: "sg-vpc-own-one",
        network: Network::First,
        groups: &[Group::First(0)],
        offenders: &[],
    },
    Row {
        case: "the five groups the member allows, all of the cluster's own VPC",
        cluster: "sg-vpc-own-five",
        network: Network::First,
        groups: &[
            Group::First(0),
            Group::First(1),
            Group::First(2),
            Group::First(3),
            Group::First(4),
        ],
        offenders: &[],
    },
    Row {
        case: "a group of another VPC the client created",
        cluster: "sg-vpc-other",
        network: Network::First,
        groups: &[Group::Second],
        offenders: &[Group::Second],
    },
    Row {
        case: "a group of the account's own default VPC, which this test did not create",
        cluster: "sg-vpc-account-default",
        network: Network::First,
        groups: &[Group::AccountDefault],
        offenders: &[Group::AccountDefault],
    },
    Row {
        case: "a group of the cluster's own VPC listed before one of another VPC",
        cluster: "sg-vpc-mixed",
        network: Network::First,
        groups: &[Group::First(1), Group::Second],
        offenders: &[Group::Second],
    },
    Row {
        case: "groups of TWO other VPCs, which the refusal lists in the order sent",
        cluster: "sg-vpc-two-offenders",
        network: Network::First,
        groups: &[Group::Second, Group::First(0), Group::AccountDefault],
        offenders: &[Group::Second, Group::AccountDefault],
    },
    Row {
        case: "a group of the FIRST VPC, for a cluster placed in the second",
        cluster: "sg-vpc-second-cluster",
        network: Network::Second,
        groups: &[Group::First(2)],
        offenders: &[Group::First(2)],
    },
    Row {
        case: "a created VPC's group, for a cluster placed in the account's default VPC",
        cluster: "sg-vpc-default-cluster",
        network: Network::Default,
        groups: &[Group::First(3)],
        offenders: &[Group::First(3)],
    },
    Row {
        case: "the default VPC's own group, for a cluster placed in that VPC",
        cluster: "sg-vpc-default-own",
        network: Network::Default,
        groups: &[Group::AccountDefault],
        offenders: &[],
    },
];

/// The VPC id and the subnet ids of one of the account's networks.
#[derive(Debug, Clone)]
struct Placement {
    vpc_id: String,
    subnets: Vec<String>,
}

/// The sentence Amazon EKS refuses security groups outside the cluster's VPC with: the
/// offending ids as the list the sentence brackets them as, and the VPC a group has to be
/// associated with to apply to the cluster.
fn refusal(group_ids: &[String], vpc_id: &str) -> String {
    format!(
        "Security group(s) [{}] are not (in/associated to) the same VPC as the subnets. \
         Please specify a security group that is associated with the VPC: {vpc_id}",
        group_ids.join(", ")
    )
}

/// Create a VPC with `subnet_cidrs` subnets of its own, through EC2 itself.
fn network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc_cidr: &str,
    subnet_cidrs: &[&str],
) -> Placement {
    let created = ec2(router, runtime, "CreateVpc", &[("CidrBlock", vpc_cidr)]);
    assert_eq!(
        created.status, 200,
        "setup: CreateVpc {vpc_cidr} must succeed, got {created:?}"
    );
    let vpc_id = element(&created.text, "vpcId");
    let mut subnets = Vec::new();
    // Each in its OWN Availability Zone: a control plane is placed across at least two
    // zones, so a cluster cannot be created in a VPC whose subnets share one. EC2 chooses
    // a zone for a subnet that names none and "we do not necessarily select a different
    // zone for each subnet", so the zone is named here, one per subnet in order.
    for (index, cidr_block) in subnet_cidrs.iter().enumerate() {
        let zone = format!(
            "{REGION}{}",
            char::from(b'a' + u8::try_from(index).unwrap_or(0))
        );
        let answer = ec2(
            router,
            runtime,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", cidr_block),
                ("AvailabilityZone", zone.as_str()),
            ],
        );
        assert_eq!(
            answer.status, 200,
            "setup: CreateSubnet {cidr_block} must succeed, got {answer:?}"
        );
        subnets.push(element(&answer.text, "subnetId"));
    }
    Placement { vpc_id, subnets }
}

/// Create one security group named `group_name` in `vpc_id`, through EC2 itself, and
/// answer the id EC2 minted for it.
fn security_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc_id: &str,
    group_name: &str,
) -> String {
    let created = ec2(
        router,
        runtime,
        "CreateSecurityGroup",
        &[
            ("VpcId", vpc_id),
            ("GroupName", group_name),
            ("GroupDescription", "a security group of one VPC"),
        ],
    );
    assert_eq!(
        created.status, 200,
        "setup: CreateSecurityGroup {group_name} must succeed, got {created:?}"
    );
    element(&created.text, "groupId")
}

/// The id of the default security group of `vpc_id` — the group EC2 creates with a VPC,
/// read back from EC2 rather than built here.
fn default_security_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc_id: &str,
) -> String {
    let described = ec2(
        router,
        runtime,
        "DescribeSecurityGroups",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc_id),
            ("Filter.2.Name", "group-name"),
            ("Filter.2.Value.1", "default"),
        ],
    );
    assert_eq!(
        described.status, 200,
        "setup: DescribeSecurityGroups must succeed, got {described:?}"
    );
    element(&described.text, "groupId")
}

/// Wait for the `CREATING` -> `ACTIVE` move the clock owes a new cluster to be
/// materialised by a read, which is what `UpdateClusterConfig` needs before it takes a
/// change.
fn wait_until_active(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    cluster: &str,
) {
    for _ in 0..POLL_ATTEMPTS {
        let read = eks(router, runtime, "GET", &format!("/clusters/{cluster}"), None);
        assert_eq!(
            read.status, 200,
            "case {case:?}: the cluster must be readable, got {read:?}"
        );
        if read.body["cluster"]["status"] == serde_json::json!("ACTIVE") {
            return;
        }
        std::thread::sleep(POLL_WAIT);
    }
    panic!("case {case:?}: the cluster never reached ACTIVE");
}

#[test]
fn eks_refuses_a_security_group_outside_the_vpc_the_cluster_sits_in() {
    std::thread::Builder::new()
        .name("eks-security-group-vpc".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // ---------------------------------------------------------------
            // Setup, all of it through EC2 itself: two VPCs with subnets of
            // their own, the five groups one row names in the first of them,
            // one group in the second, and the account's own default network
            // with the default group it came with.
            // ---------------------------------------------------------------
            let first = network(
                &router,
                &runtime,
                "10.31.0.0/16",
                &["10.31.1.0/24", "10.31.2.0/24"],
            );
            let second = network(
                &router,
                &runtime,
                "10.32.0.0/16",
                &["10.32.1.0/24", "10.32.2.0/24"],
            );
            let first_groups: Vec<String> = (0..MAX_SECURITY_GROUPS)
                .map(|index| {
                    security_group(
                        &router,
                        &runtime,
                        &first.vpc_id,
                        &format!("eks-vpc-first-{index}"),
                    )
                })
                .collect();
            let second_group =
                security_group(&router, &runtime, &second.vpc_id, "eks-vpc-second-0");

            let defaults = ec2(
                &router,
                &runtime,
                "DescribeSubnets",
                &[
                    ("Filter.1.Name", "default-for-az"),
                    ("Filter.1.Value.1", "true"),
                ],
            );
            assert_eq!(
                defaults.status, 200,
                "setup: DescribeSubnets must succeed, got {defaults:?}"
            );
            let default_subnets = elements(&defaults.text, "subnetId");
            assert!(
                default_subnets.len() >= 2,
                "setup: the account must have at least two default subnets, got \
                 {default_subnets:?}"
            );
            let default_placement = Placement {
                vpc_id: element(&defaults.text, "vpcId"),
                subnets: default_subnets,
            };
            let default_group =
                default_security_group(&router, &runtime, &default_placement.vpc_id);

            let placement = |network: Network| -> &Placement {
                match network {
                    Network::First => &first,
                    Network::Second => &second,
                    Network::Default => &default_placement,
                }
            };
            let group_id = |group: Group| -> String {
                match group {
                    Group::First(index) => first_groups
                        .get(index)
                        .expect("every row names a group the setup created")
                        .clone(),
                    Group::Second => second_group.clone(),
                    Group::AccountDefault => default_group.clone(),
                }
            };

            // ---------------------------------------------------------------
            // RULE, on CreateCluster: a group outside the VPC the cluster's
            // subnets sit in is refused, and nothing is created for it.
            // ---------------------------------------------------------------
            for row in ROWS {
                let network = placement(row.network);
                let groups: Vec<String> =
                    row.groups.iter().map(|group| group_id(*group)).collect();
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    "/clusters",
                    Some(serde_json::json!({
                        "name": row.cluster,
                        "roleArn": role_arn("eksClusterRole"),
                        "resourcesVpcConfig": {
                            "subnetIds": network.subnets,
                            "securityGroupIds": groups,
                        },
                    })),
                );
                let read = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/clusters/{}", row.cluster),
                    None,
                );
                let offenders: Vec<String> = row
                    .offenders
                    .iter()
                    .map(|group| group_id(*group))
                    .collect();
                if offenders.is_empty() {
                    assert_eq!(
                        answer.status, 200,
                        "case {:?}: groups of the cluster's own VPC must be served, got \
                         {answer:?}",
                        row.case
                    );
                    assert_eq!(
                        read.status, 200,
                        "case {:?}: the cluster must read back, got {read:?}",
                        row.case
                    );
                    assert_eq!(
                        read.body["cluster"]["resourcesVpcConfig"]["securityGroupIds"],
                        serde_json::json!(groups),
                        "case {:?}: the cluster must report the groups it was created \
                         with, got {}",
                        row.case,
                        read.text
                    );
                    assert_eq!(
                        read.body["cluster"]["resourcesVpcConfig"]["vpcId"],
                        serde_json::json!(network.vpc_id),
                        "case {:?}: the cluster must sit in the VPC its subnets belong \
                         to, got {}",
                        row.case,
                        read.text
                    );
                } else {
                    assert_eq!(
                        (answer.status, answer.error_type.as_deref()),
                        (400, Some("InvalidParameterException")),
                        "case {:?}: a security group outside the cluster's VPC must be \
                         refused 400 InvalidParameterException, got {answer:?}",
                        row.case
                    );
                    let expected = refusal(&offenders, &network.vpc_id);
                    assert_eq!(
                        answer.message(),
                        expected,
                        "case {:?}: expected message {expected:?}, got {:?}",
                        row.case,
                        answer.message()
                    );
                    assert_eq!(
                        (read.status, read.error_type.as_deref()),
                        (404, Some("ResourceNotFoundException")),
                        "case {:?}: a refused create must leave no cluster, got {read:?}",
                        row.case
                    );
                }
            }

            // ---------------------------------------------------------------
            // The SAME rule on UpdateClusterConfig, where the network is the
            // VPC the cluster already reports: a group of another VPC is
            // refused and starts no update, one of its own VPC is taken.
            // ---------------------------------------------------------------
            let cluster = "sg-vpc-update";
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(serde_json::json!({
                    "name": cluster,
                    "roleArn": role_arn("eksClusterRole"),
                    "resourcesVpcConfig": { "subnetIds": first.subnets },
                })),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster an update changes must be created, got {created:?}"
            );
            wait_until_active(&router, &runtime, "the update's cluster", cluster);

            let update_uri = format!("/clusters/{cluster}/update-config");
            let refused = eks(
                &router,
                &runtime,
                "POST",
                &update_uri,
                Some(serde_json::json!({
                    "resourcesVpcConfig": { "securityGroupIds": [second_group] },
                })),
            );
            let case = "an update naming a group of another VPC";
            assert_eq!(
                (refused.status, refused.error_type.as_deref()),
                (400, Some("InvalidParameterException")),
                "case {case:?}: must be refused 400 InvalidParameterException, got {refused:?}"
            );
            let expected = refusal(std::slice::from_ref(&second_group), &first.vpc_id);
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                refused.message()
            );
            let unchanged = eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None);
            assert_eq!(
                unchanged.body["cluster"]["status"],
                serde_json::json!("ACTIVE"),
                "case {case:?}: a refused update must start none, got {}",
                unchanged.text
            );

            let case = "an update naming a group of the cluster's own VPC";
            let accepted = eks(
                &router,
                &runtime,
                "POST",
                &update_uri,
                Some(serde_json::json!({
                    "resourcesVpcConfig": { "securityGroupIds": [first_groups[0]] },
                })),
            );
            assert_eq!(
                accepted.status, 200,
                "case {case:?}: must be served, got {accepted:?}"
            );
            assert!(
                accepted.body["update"]["id"].is_string(),
                "case {case:?}: the answer must carry the update it started, got {}",
                accepted.text
            );
            let updating = eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None);
            assert_eq!(
                updating.body["cluster"]["status"],
                serde_json::json!("UPDATING"),
                "case {case:?}: the cluster must be reported under the update, got {}",
                updating.text
            );
        })
        .expect("spawn the eks security-group-vpc thread")
        .join()
        .expect("the eks security-group-vpc thread");
}
