// SPDX-License-Identifier: CC0-1.0

//! Every subnet an Amazon EKS resource is placed in has to sit in ONE VPC.
//!
//! RULE under test: Amazon EKS provisions a resource's network in a single VPC — the
//! cross-account elastic network interfaces of a control plane, the cluster's own
//! security group, a node group's instances and a Fargate Pod's addresses all live in
//! the network that resource's subnets sit in — and a subnet belongs to exactly one VPC
//! (`Subnet$VpcId`). So a `subnetIds`/`subnets` list whose subnets, as EC2 reports them,
//! are spread across more than one VPC names no network to provision in at all: it is
//! refused `InvalidParameterException` (HTTP 400) with "Subnets specified must belong to
//! the same VPC", and nothing is created or changed. A list that reaches one VPC is
//! served, whichever VPC that is and however the list spells it.
//!
//! The VPC a subnet belongs to is Amazon EC2's to report, which is why this suite lives
//! here rather than in the EKS crate: only an emulator with EC2 mounted beside Amazon EKS
//! can answer the question at all, and an emulator with no EC2 refuses nothing (the EKS
//! crate's own `peer_resolution_is_grounded` pins that half).
//!
//! The table spans what the rule generalises over: how many ids the list carries (two,
//! three), how many networks they reach (one, two), WHICH networks (two the client
//! created and the account's own default VPC, which nothing here created), where the
//! offending id sits in the list (first, last), whether an id repeats, and whether the
//! two networks' subnets sit in different Availability Zones or in the same one. Each
//! refused row is then shown to leave no cluster — neither readable nor listed — and each
//! served row to report the subnets it named and the one VPC EC2 placed them in.
//!
//! The same reading of a subnet list governs every operation that names one, so the suite
//! closes on the other three: `CreateNodegroup`, `CreateFargateProfile` and
//! `UpdateClusterConfig` — a cross-VPC list refused with the same sentence and nothing
//! written, a single-VPC list served.
//!
//! Nothing sleeps for a due instant inside the emulator, so a clock-owed status move is
//! POLLED until it holds, with a generous deadline.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.
//!
//! The request harness is the test crate's own: `eks_wire` for the drivers and the answer,
//! `query_wire` for the XML readers.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::eks_wire::{cluster_body, ec2, eks, role_arn, Answer, REGION};
use crate::query_wire::{element, elements};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The whole sentence Amazon EKS refuses a subnet list spread over two VPCs with — fixed
/// text, no variable part.
const ONE_VPC: &str = "Subnets specified must belong to the same VPC";

/// How often a clock-owed read is retried, and how many times: nothing inside the
/// emulator sleeps for a due instant, so a status is polled rather than waited for, with
/// a deadline far above any span this service owes.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 400 polls at 50ms is a 20-second deadline.
const POLLS: usize = 400;

/// Poll `read` until `holds` answers true and answer the last read; panic naming `what`
/// when the deadline passes first.
fn read_until(
    what: &str,
    mut read: impl FnMut() -> Answer,
    holds: impl Fn(&Answer) -> bool,
) -> Answer {
    let mut last = read();
    for _ in 0..POLLS {
        if holds(&last) {
            return last;
        }
        std::thread::sleep(POLL_INTERVAL);
        last = read();
    }
    panic!("expected {what}; the last read answered {last:?}");
}

/// Which of the account's networks a picked subnet comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Net {
    /// The first VPC this suite created.
    First,
    /// A second VPC this suite created, so no row is about one VPC.
    Second,
    /// The account's own default VPC, which nothing in this suite created.
    Default,
}

/// One subnet of one network: which network, and which of its subnets in the order the
/// setup laid them out.
#[derive(Debug, Clone, Copy)]
struct Pick(Net, usize);

/// One row: the subnets a create names, and the network it must end up in — `None` when
/// the list reaches more than one network and Amazon EKS refuses it.
struct Row {
    case: &'static str,
    cluster: &'static str,
    subnets: &'static [Pick],
    network: Option<Net>,
}

/// The setup lays out, per network, subnets in the zones `[a, b, a]`: so a row can name
/// two networks whose subnets sit in two zones, and another whose subnets sit in the
/// SAME zone.
const ROWS: &[Row] = &[
    Row {
        case: "two subnets of one VPC",
        cluster: "one-vpc-pair",
        subnets: &[Pick(Net::First, 0), Pick(Net::First, 1)],
        network: Some(Net::First),
    },
    Row {
        case: "three subnets of one VPC with an id repeated",
        cluster: "one-vpc-repeat",
        subnets: &[
            Pick(Net::First, 0),
            Pick(Net::First, 1),
            Pick(Net::First, 0),
        ],
        network: Some(Net::First),
    },
    Row {
        case: "two subnets of the OTHER VPC this suite created",
        cluster: "one-vpc-second",
        subnets: &[Pick(Net::Second, 0), Pick(Net::Second, 1)],
        network: Some(Net::Second),
    },
    Row {
        case: "two subnets of the account's own default VPC, which this suite did not create",
        cluster: "one-vpc-default",
        subnets: &[Pick(Net::Default, 0), Pick(Net::Default, 1)],
        network: Some(Net::Default),
    },
    Row {
        case: "one subnet from each of two VPCs",
        cluster: "two-vpc-pair",
        subnets: &[Pick(Net::First, 0), Pick(Net::Second, 1)],
        network: None,
    },
    Row {
        case: "two subnets of one VPC and a third of another, the offender last",
        cluster: "two-vpc-offender-last",
        subnets: &[
            Pick(Net::First, 0),
            Pick(Net::First, 1),
            Pick(Net::Second, 1),
        ],
        network: None,
    },
    Row {
        case: "the offender FIRST in the list",
        cluster: "two-vpc-offender-first",
        subnets: &[Pick(Net::Second, 0), Pick(Net::First, 1)],
        network: None,
    },
    Row {
        case: "subnets of two VPCs that sit in the SAME Availability Zone",
        cluster: "two-vpc-one-zone",
        subnets: &[Pick(Net::First, 0), Pick(Net::Second, 0)],
        network: None,
    },
    Row {
        case: "a subnet of a created VPC beside one of the account's default VPC",
        cluster: "two-vpc-with-default",
        subnets: &[Pick(Net::First, 0), Pick(Net::Default, 0)],
        network: None,
    },
    Row {
        case: "a subnet of the account's default VPC listed first",
        cluster: "two-vpc-default-first",
        subnets: &[Pick(Net::Default, 1), Pick(Net::Second, 1)],
        network: None,
    },
];

/// The VPC id and the subnet ids of one of the account's networks.
#[derive(Debug, Clone)]
struct Placement {
    vpc_id: String,
    subnets: Vec<String>,
}

/// Create a VPC with three subnets of its own, in the zones `[a, b, a]`, through EC2
/// itself.
///
/// The zone is NAMED rather than left to EC2: a control plane is placed across at least
/// two Availability Zones, so every row that reaches one VPC has to span two of them, and
/// the one row about two VPCs in a single zone needs that zone to be the same in both
/// networks. EC2 "does not necessarily select a different zone for each subnet" when a
/// request names none.
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
    let zones = [format!("{REGION}a"), format!("{REGION}b"), format!("{REGION}a")];
    let mut subnets = Vec::new();
    for (index, cidr_block) in subnet_cidrs.iter().enumerate() {
        let zone = zones
            .get(index)
            .expect("the setup names a zone for every subnet it creates");
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
            "setup: CreateSubnet {cidr_block} in {zone} must succeed, got {answer:?}"
        );
        // EC2 reports the network and the zone it placed the subnet in, so the layout the
        // rows are written against is the one EC2 actually holds.
        assert_eq!(
            element(&answer.text, "availabilityZone"),
            *zone,
            "setup: CreateSubnet {cidr_block} must sit in {zone}, got {answer:?}"
        );
        assert_eq!(
            element(&answer.text, "vpcId"),
            vpc_id,
            "setup: CreateSubnet {cidr_block} must sit in {vpc_id}, got {answer:?}"
        );
        subnets.push(element(&answer.text, "subnetId"));
    }
    Placement { vpc_id, subnets }
}

/// The account's own default network, read back from EC2 rather than created here: the
/// default subnets of the Region and the VPC they belong to.
fn default_network(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Placement {
    let described = ec2(
        router,
        runtime,
        "DescribeSubnets",
        &[("Filter.1.Name", "default-for-az"), ("Filter.1.Value.1", "true")],
    );
    assert_eq!(
        described.status, 200,
        "setup: DescribeSubnets must succeed, got {described:?}"
    );
    let subnets = elements(&described.text, "subnetId");
    assert!(
        subnets.len() >= 2,
        "setup: the account must have at least two default subnets, got {subnets:?}"
    );
    Placement {
        vpc_id: element(&described.text, "vpcId"),
        subnets,
    }
}

#[test]
fn eks_refuses_subnets_drawn_from_more_than_one_vpc() {
    std::thread::Builder::new()
        .name("eks-subnets-one-vpc".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // Setup, all of it through EC2 itself: two VPCs of this suite's own, plus
            // the account's default network, which nothing here created.
            let first = network(
                &router,
                &runtime,
                "10.41.0.0/16",
                &["10.41.1.0/24", "10.41.2.0/24", "10.41.3.0/24"],
            );
            let second = network(
                &router,
                &runtime,
                "10.42.0.0/16",
                &["10.42.1.0/24", "10.42.2.0/24", "10.42.3.0/24"],
            );
            let default = default_network(&router, &runtime);

            let placement = |net: Net| -> &Placement {
                match net {
                    Net::First => &first,
                    Net::Second => &second,
                    Net::Default => &default,
                }
            };
            let subnet_id = |pick: Pick| -> String {
                placement(pick.0)
                    .subnets
                    .get(pick.1)
                    .expect("every row names a subnet the setup laid out")
                    .clone()
            };
            let named = |picks: &[Pick]| -> Vec<String> {
                picks.iter().map(|pick| subnet_id(*pick)).collect()
            };

            // ---------------------------------------------------------------
            // RULE, on CreateCluster: a subnet list that reaches more than one
            // VPC is refused, and nothing is created for it.
            // ---------------------------------------------------------------
            for row in ROWS {
                let subnets = named(row.subnets);
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    "/clusters",
                    Some(cluster_body(row.cluster, &subnets)),
                );
                let read = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/clusters/{}", row.cluster),
                    None,
                );
                let listed = eks(&router, &runtime, "GET", "/clusters", None);
                assert_eq!(
                    listed.status, 200,
                    "case {:?}: ListClusters must answer, got {listed:?}",
                    row.case
                );
                let names = listed.body["clusters"].to_string();
                match row.network {
                    None => {
                        assert_eq!(
                            (answer.status, answer.error_type.as_deref()),
                            (400, Some("InvalidParameterException")),
                            "case {:?}: subnets of two VPCs must be refused 400 \
                             InvalidParameterException, got {answer:?}",
                            row.case
                        );
                        assert_eq!(
                            answer.message(),
                            ONE_VPC,
                            "case {:?}: expected message {ONE_VPC:?}, got {:?}",
                            row.case,
                            answer.message()
                        );
                        assert_eq!(
                            (read.status, read.error_type.as_deref()),
                            (404, Some("ResourceNotFoundException")),
                            "case {:?}: a refused create must leave no cluster, got {read:?}",
                            row.case
                        );
                        assert!(
                            !names.contains(row.cluster),
                            "case {:?}: a refused create must leave the name unlisted, got \
                             {names}",
                            row.case
                        );
                    }
                    Some(net) => {
                        assert_eq!(
                            answer.status, 200,
                            "case {:?}: subnets of one VPC must be served, got {answer:?} ({})",
                            row.case,
                            answer.message()
                        );
                        // Round trip: the cluster reads back with the subnets it was
                        // created with and the ONE VPC EC2 says they sit in.
                        assert_eq!(
                            read.status, 200,
                            "case {:?}: the cluster must read back, got {read:?}",
                            row.case
                        );
                        assert_eq!(
                            read.body["cluster"]["resourcesVpcConfig"]["subnetIds"],
                            serde_json::json!(subnets),
                            "case {:?}: the cluster must report the subnets it was created \
                             with, got {}",
                            row.case,
                            read.text
                        );
                        assert_eq!(
                            read.body["cluster"]["resourcesVpcConfig"]["vpcId"],
                            serde_json::json!(placement(net).vpc_id),
                            "case {:?}: the cluster must sit in the VPC its subnets belong to, \
                             got {}",
                            row.case,
                            read.text
                        );
                        assert!(
                            names.contains(row.cluster),
                            "case {:?}: a served create must list the cluster, got {names}",
                            row.case
                        );
                    }
                }
            }

            // ---------------------------------------------------------------
            // The SAME reading on the other operations that name subnets. The
            // host cluster sits wholly in the first VPC.
            // ---------------------------------------------------------------
            let cluster = "one-vpc-children-host";
            let host_subnets = named(&[Pick(Net::First, 0), Pick(Net::First, 1)]);
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &host_subnets)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the host cluster must be created, got {created:?} ({})",
                created.message()
            );
            // Its children and its own update are taken against a RUNNING control plane,
            // so the clock-owed CREATING -> ACTIVE move is polled for first.
            read_until(
                "the host cluster to reach ACTIVE",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| answer.body["cluster"]["status"] == serde_json::json!("ACTIVE"),
            );

            // CreateNodegroup: the instances of a node group are launched into one
            // network too.
            let nodegroup_rows: [(&str, &str, Vec<String>, bool); 2] = [
                (
                    "a node group in subnets of two VPCs",
                    "nodes-two-vpcs",
                    named(&[Pick(Net::First, 0), Pick(Net::Second, 1)]),
                    true,
                ),
                (
                    "a node group in subnets of one VPC",
                    "nodes-one-vpc",
                    named(&[Pick(Net::First, 1), Pick(Net::First, 2)]),
                    false,
                ),
            ];
            for (case, nodegroup, subnets, refused) in &nodegroup_rows {
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/clusters/{cluster}/node-groups"),
                    Some(serde_json::json!({
                        "nodegroupName": nodegroup,
                        "nodeRole": role_arn("eksNodeRole"),
                        "subnets": subnets,
                    })),
                );
                let read = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
                    None,
                );
                if *refused {
                    assert_eq!(
                        (answer.status, answer.error_type.as_deref()),
                        (400, Some("InvalidParameterException")),
                        "case {case:?}: must be refused 400 InvalidParameterException, got \
                         {answer:?}"
                    );
                    assert_eq!(
                        answer.message(),
                        ONE_VPC,
                        "case {case:?}: expected message {ONE_VPC:?}, got {:?}",
                        answer.message()
                    );
                    assert_eq!(
                        (read.status, read.error_type.as_deref()),
                        (404, Some("ResourceNotFoundException")),
                        "case {case:?}: a refused create must leave no node group, got {read:?}"
                    );
                } else {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: must be served, got {answer:?} ({})",
                        answer.message()
                    );
                    assert_eq!(
                        read.body["nodegroup"]["subnets"],
                        serde_json::json!(subnets),
                        "case {case:?}: the node group must report its subnets, got {}",
                        read.text
                    );
                }
            }

            // CreateFargateProfile: a Fargate Pod's network interface is created in one
            // network as well.
            let fargate_rows: [(&str, &str, Vec<String>, bool); 2] = [
                (
                    "a Fargate profile in subnets of two VPCs",
                    "fp-two-vpcs",
                    named(&[Pick(Net::First, 0), Pick(Net::Second, 1)]),
                    true,
                ),
                (
                    "a Fargate profile in subnets of one VPC",
                    "fp-one-vpc",
                    named(&[Pick(Net::First, 0), Pick(Net::First, 2)]),
                    false,
                ),
            ];
            for (case, profile, subnets, refused) in &fargate_rows {
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/clusters/{cluster}/fargate-profiles"),
                    Some(serde_json::json!({
                        "fargateProfileName": profile,
                        "podExecutionRoleArn": role_arn("eksFargatePodExecutionRole"),
                        "subnets": subnets,
                        "selectors": [{ "namespace": "default" }],
                    })),
                );
                let read = eks(
                    &router,
                    &runtime,
                    "GET",
                    &format!("/clusters/{cluster}/fargate-profiles/{profile}"),
                    None,
                );
                if *refused {
                    assert_eq!(
                        (answer.status, answer.error_type.as_deref()),
                        (400, Some("InvalidParameterException")),
                        "case {case:?}: must be refused 400 InvalidParameterException, got \
                         {answer:?}"
                    );
                    assert_eq!(
                        answer.message(),
                        ONE_VPC,
                        "case {case:?}: expected message {ONE_VPC:?}, got {:?}",
                        answer.message()
                    );
                    assert_eq!(
                        (read.status, read.error_type.as_deref()),
                        (404, Some("ResourceNotFoundException")),
                        "case {case:?}: a refused create must leave no profile, got {read:?}"
                    );
                } else {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: must be served, got {answer:?} ({})",
                        answer.message()
                    );
                    assert_eq!(
                        read.body["fargateProfile"]["subnets"],
                        serde_json::json!(subnets),
                        "case {case:?}: the profile must report its subnets, got {}",
                        read.text
                    );
                }
            }

            // UpdateClusterConfig, which also chooses the subnets a control plane is
            // placed in: a cross-VPC list is refused and changes nothing.
            let update_uri = format!("/clusters/{cluster}/update-config");
            let case = "an update onto subnets of two VPCs";
            let refused = eks(
                &router,
                &runtime,
                "POST",
                &update_uri,
                Some(serde_json::json!({
                    "resourcesVpcConfig": {
                        "subnetIds": named(&[Pick(Net::First, 1), Pick(Net::Second, 1)]),
                    },
                })),
            );
            assert_eq!(
                (refused.status, refused.error_type.as_deref()),
                (400, Some("InvalidParameterException")),
                "case {case:?}: must be refused 400 InvalidParameterException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                ONE_VPC,
                "case {case:?}: expected message {ONE_VPC:?}, got {:?}",
                refused.message()
            );
            let unchanged = eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None);
            assert_eq!(
                unchanged.body["cluster"]["resourcesVpcConfig"]["subnetIds"],
                serde_json::json!(host_subnets),
                "case {case:?}: a refused update must change nothing, got {}",
                unchanged.text
            );
            assert_eq!(
                unchanged.body["cluster"]["status"],
                serde_json::json!("ACTIVE"),
                "case {case:?}: a refused update must start none, got {}",
                unchanged.text
            );

            let case = "an update onto other subnets of the cluster's own VPC";
            let moved = named(&[Pick(Net::First, 1), Pick(Net::First, 2)]);
            let accepted = eks(
                &router,
                &runtime,
                "POST",
                &update_uri,
                Some(serde_json::json!({
                    "resourcesVpcConfig": { "subnetIds": moved },
                })),
            );
            assert_eq!(
                accepted.status, 200,
                "case {case:?}: must be served, got {accepted:?} ({})",
                accepted.message()
            );
            let applied = read_until(
                "the cluster to report the subnets the update chose",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| {
                    answer.body["cluster"]["resourcesVpcConfig"]["subnetIds"]
                        == serde_json::json!(moved)
                },
            );
            assert_eq!(
                applied.body["cluster"]["resourcesVpcConfig"]["subnetIds"],
                serde_json::json!(moved),
                "case {case:?}: the update must be applied, got {}",
                applied.text
            );
        })
        .expect("spawn the eks subnets-one-vpc thread")
        .join()
        .expect("the eks subnets-one-vpc thread");
}
