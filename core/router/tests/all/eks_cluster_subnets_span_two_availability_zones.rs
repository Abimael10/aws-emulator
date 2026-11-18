// SPDX-License-Identifier: CC0-1.0

//! An Amazon EKS cluster's subnets must place its control plane in at least TWO
//! Availability Zones, and the zones are read from EC2 rather than counted off the
//! request's list.
//!
//! RULE under test: a `resourcesVpcConfig.subnetIds` whose subnets — as EC2 reports them —
//! sit in fewer than two distinct Availability Zones is refused `InvalidParameterException`
//! (HTTP 400) with "Subnets specified must be in at least two different AZs", and nothing
//! is created; a list whose subnets span two or more zones is served, however many ids it
//! carries and however many of them repeat. "You must specify at least two subnets that
//! are in different Availability Zones"
//! (`smithy_query(query="shape", shape="VpcConfigRequest")`).
//!
//! That makes the ZONES, not the number of ids, the requirement, which is why this suite
//! lives here rather than in the EKS crate: a subnet's zone is Amazon EC2's to report, so
//! only an emulator with EC2 mounted beside Amazon EKS can answer the question at all.
//!
//! The table spans what the rule generalises over: how many ids the list carries (one, two,
//! three), whether they are distinct or repeated, and how their zones spread (all in one
//! zone, two zones, three zones, two zones that the single-zone rows never name). Each
//! refused row is then shown to leave no cluster — neither readable nor listed — and each
//! served row to store the subnets it named and the VPC they sit in.
//!
//! The same rule on `UpdateClusterConfig`, which also "can be used to choose different
//! subnets", closes the suite: an update onto one zone is refused with the same sentence
//! and changes nothing, and an update onto two is taken and applied.
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

use crate::eks_wire::{cluster_body, ec2, eks, Answer, REGION};
use crate::query_wire::element;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The whole sentence Amazon EKS refuses a single-zone placement with — fixed text, no
/// variable part.
const TWO_AZS: &str = "Subnets specified must be in at least two different AZs";

/// How often a clock-owed read is retried, and how many times: nothing inside the emulator
/// sleeps for a due instant, so a status is polled rather than waited for, with a deadline
/// far above any span this service owes.
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

/// One row: which of the setup's subnets a create names, and whether Amazon EKS must
/// refuse it for the zones they sit in.
struct Row {
    case: &'static str,
    cluster: &'static str,
    /// Indices into the subnets the setup created, in the order the request lists them —
    /// a repeated index is the same subnet id named twice.
    subnets: &'static [usize],
    /// Whether the placement those subnets describe must be refused.
    refused: bool,
}

/// The setup lays out four subnets of ONE VPC: two in the first zone, one in the second,
/// one in the third. Every row is a selection of them, so each row differs from the others
/// only in the zones its selection spans and in how it spells that selection.
const ROWS: &[Row] = &[
    Row {
        case: "two DISTINCT subnets that sit in the same zone",
        cluster: "az-two-subnets-one-zone",
        subnets: &[0, 1],
        refused: true,
    },
    Row {
        case: "the same subnet id twice",
        cluster: "az-one-subnet-twice",
        subnets: &[0, 0],
        refused: true,
    },
    Row {
        case: "three distinct subnets that all sit in the same zone",
        cluster: "az-three-subnets-one-zone",
        subnets: &[0, 1, 0],
        refused: true,
    },
    Row {
        case: "a single subnet, which cannot be in two zones",
        cluster: "az-single-subnet",
        subnets: &[2],
        refused: true,
    },
    Row {
        case: "two subnets, one in each of two zones",
        cluster: "az-two-zones",
        subnets: &[0, 2],
        refused: false,
    },
    Row {
        case: "three subnets whose zones are the second and the third",
        cluster: "az-other-two-zones",
        subnets: &[2, 3, 2],
        refused: false,
    },
    Row {
        case: "three subnets spanning three zones",
        cluster: "az-three-zones",
        subnets: &[1, 2, 3],
        refused: false,
    },
    Row {
        case: "a repeated id beside a subnet of another zone",
        cluster: "az-repeat-plus-other-zone",
        subnets: &[1, 1, 3],
        refused: false,
    },
];

/// Create the VPC and its four subnets through EC2 itself, and answer the subnet ids in
/// the order the zones above describe: `[zone a, zone a, zone b, zone c]`.
fn given_subnets(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Vec<String> {
    let created = ec2(router, runtime, "CreateVpc", &[("CidrBlock", "10.61.0.0/16")]);
    assert_eq!(
        created.status, 200,
        "setup: CreateVpc must succeed, got {created:?}"
    );
    let vpc_id = element(&created.text, "vpcId");
    let zone_a = format!("{REGION}a");
    let zone_b = format!("{REGION}b");
    let zone_c = format!("{REGION}c");
    let layout = [
        ("10.61.1.0/24", zone_a.as_str()),
        ("10.61.2.0/24", zone_a.as_str()),
        ("10.61.3.0/24", zone_b.as_str()),
        ("10.61.4.0/24", zone_c.as_str()),
    ];
    let mut subnets = Vec::new();
    for (cidr_block, zone) in layout {
        let answer = ec2(
            router,
            runtime,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", cidr_block),
                ("AvailabilityZone", zone),
            ],
        );
        assert_eq!(
            answer.status, 200,
            "setup: CreateSubnet {cidr_block} in {zone} must succeed, got {answer:?}"
        );
        // EC2 reports the zone it placed the subnet in, so the layout the rows are
        // written against is the one EC2 actually holds.
        assert_eq!(
            element(&answer.text, "availabilityZone"),
            zone,
            "setup: CreateSubnet {cidr_block} must sit in {zone}, got {answer:?}"
        );
        subnets.push(element(&answer.text, "subnetId"));
    }
    subnets
}

#[test]
fn a_cluster_is_refused_unless_its_subnets_span_two_availability_zones() {
    std::thread::Builder::new()
        .name("eks-cluster-subnet-zones".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let held = given_subnets(&router, &runtime);

            for row in ROWS {
                let named: Vec<String> = row
                    .subnets
                    .iter()
                    .map(|index| {
                        held.get(*index)
                            .expect("every row names a subnet the setup created")
                            .clone()
                    })
                    .collect();
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    "/clusters",
                    Some(cluster_body(row.cluster, &named)),
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
                if row.refused {
                    assert_eq!(
                        (answer.status, answer.error_type.as_deref()),
                        (400, Some("InvalidParameterException")),
                        "case {:?}: a single-zone placement must be refused 400 \
                         InvalidParameterException, got {answer:?}",
                        row.case
                    );
                    assert_eq!(
                        answer.message(),
                        TWO_AZS,
                        "case {:?}: expected message {TWO_AZS:?}, got {:?}",
                        row.case,
                        answer.message()
                    );
                    // And nothing was created: the cluster is neither readable nor listed.
                    assert_eq!(
                        (read.status, read.error_type.as_deref()),
                        (404, Some("ResourceNotFoundException")),
                        "case {:?}: a refused create must leave no cluster, got {read:?}",
                        row.case
                    );
                    assert!(
                        !names.contains(row.cluster),
                        "case {:?}: a refused create must leave the name unlisted, got {names}",
                        row.case
                    );
                } else {
                    assert_eq!(
                        answer.status, 200,
                        "case {:?}: subnets spanning two zones must be served, got {answer:?} \
                         ({})",
                        row.case,
                        answer.message()
                    );
                    assert_eq!(
                        answer.body["cluster"]["status"],
                        serde_json::json!("CREATING"),
                        "case {:?}: a new cluster is CREATING, got {}",
                        row.case,
                        answer.text
                    );
                    // Round trip: the cluster reads back with the subnets it was created
                    // with and the VPC EC2 says they sit in.
                    assert_eq!(
                        read.status, 200,
                        "case {:?}: the cluster must read back, got {read:?}",
                        row.case
                    );
                    assert_eq!(
                        read.body["cluster"]["resourcesVpcConfig"]["subnetIds"],
                        serde_json::json!(named),
                        "case {:?}: the cluster must report the subnets it was created \
                         with, got {}",
                        row.case,
                        read.text
                    );
                    assert!(
                        read.body["cluster"]["resourcesVpcConfig"]["vpcId"].is_string(),
                        "case {:?}: the cluster must report the VPC its subnets sit in, \
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

            // ---------------------------------------------------------------
            // The SAME rule on UpdateClusterConfig, which also chooses the
            // subnets a control plane is placed in.
            // ---------------------------------------------------------------
            let cluster = "az-update-host";
            let two_zones = vec![held[0].clone(), held[2].clone()];
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &two_zones)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster an update changes must be created, got {created:?}"
            );
            read_until(
                "the update's cluster to reach ACTIVE",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| answer.body["cluster"]["status"] == serde_json::json!("ACTIVE"),
            );

            let update_uri = format!("/clusters/{cluster}/update-config");
            let one_zone = vec![held[0].clone(), held[1].clone()];
            let refused = eks(
                &router,
                &runtime,
                "POST",
                &update_uri,
                Some(serde_json::json!({
                    "resourcesVpcConfig": { "subnetIds": one_zone },
                })),
            );
            let case = "an update onto two subnets of one zone";
            assert_eq!(
                (refused.status, refused.error_type.as_deref()),
                (400, Some("InvalidParameterException")),
                "case {case:?}: must be refused 400 InvalidParameterException, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                TWO_AZS,
                "case {case:?}: expected message {TWO_AZS:?}, got {:?}",
                refused.message()
            );
            let unchanged = eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None);
            assert_eq!(
                unchanged.body["cluster"]["resourcesVpcConfig"]["subnetIds"],
                serde_json::json!(two_zones),
                "case {case:?}: a refused update must change nothing, got {}",
                unchanged.text
            );

            let case = "an update onto subnets of two zones";
            let moved = vec![held[1].clone(), held[3].clone()];
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
        .expect("spawn the eks cluster-subnet-zones thread")
        .join()
        .expect("the eks cluster-subnet-zones thread");
}
