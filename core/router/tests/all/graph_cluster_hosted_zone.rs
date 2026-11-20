// SPDX-License-Identifier: CC0-1.0

//! The Route 53 hosted zone a graph-engine DB cluster reports, through BOTH front doors
//! that serve this engine's control plane.
//!
//! The rule under test: `DBCluster$HostedZoneId` is "the ID that Amazon Route 53 assigns
//! when you create a hosted zone" — there is ONE zone per region and per the DNS domain of
//! the control plane that owns the engine, so every cluster of one engine in one region
//! reports the SAME id, the id AWS published for that region wherever the emulator can cite
//! one, and never a value derived per deployment, per account or per cluster. The id is a
//! property of the region, so every operation that answers with the cluster reports it, and
//! so does the endpoint of every DB instance in that cluster (`Endpoint$HostedZoneId`, the
//! same member of the same zone).
//!
//! This suite needs the WHOLE router because the rule spans two mounted services: the graph
//! control plane's own front door (reached under the routing key `neptune`) and the shared
//! Query front door its model's signing name resolves to (`rds`), which serves the same
//! engine's clusters. One zone per region and domain holds however a caller reached the
//! control plane, so the two doors must report the same id for a region — the published one
//! where AWS publishes it, and otherwise the same minted stand-in, stable across reads and
//! invariant across the clusters of the region, which is what a client resolving an endpoint
//! through the member needs.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;
use tokio::runtime::Runtime;

use crate::query_wire::{element, elements, form_encode, opt_element};
use crate::rds_docdb_neptune_signing_name_routing::{API_VERSION, BOOT_STACK_SIZE};

/// The engine whose clusters this suite reads.
const ENGINE: &str = "neptune";

/// The DB instance class a cluster member is created with: one the orderable-options report
/// offers for a cluster of this engine.
const MEMBER_CLASS: &str = "db.r5.large";

/// The status a cluster reaches once the transition its create owes it has settled, which is
/// the state a modification is accepted in.
const AVAILABLE: &str = "available";

/// How long a poll waits for a clock-owed transition before this suite calls it
/// unobservable. Deliberately far larger than any span a service here stores: the test waits
/// for the CONDITION, so a loaded host cannot turn the wait into a failure.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll pauses between looks. Nothing is asserted about how many looks it took.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One front door this engine's control plane is reached under: the routing key a request's
/// credential scope names, and whether that door serves `ModifyDBCluster` — the graph door
/// answers 501 for it (its own stub issue), so there is no cluster report to read there.
struct Door {
    key: &'static str,
    serves_modify: bool,
}

const DOORS: &[Door] = &[
    Door {
        key: "neptune",
        serves_modify: false,
    },
    Door {
        key: "rds",
        serves_modify: true,
    },
];

/// One row of the hosted-zone table: the region a cluster is created in and the id AWS
/// publishes for that region's endpoints of this engine, or `None` for a region this
/// emulator cites no published id for.
///
/// The published id is the "Amazon Neptune" row of the hosted zone ID table in the Amazon
/// Route 53 Developer Guide ("Values that you specify when you create alias resource record
/// sets"), written out here rather than read from a service's own table so the assertion is
/// independent of the code under test. The rows span both kinds of region — one with a
/// published id and two without — and the regions differ in partition-wide prefix, so no row
/// is a rename of another.
const ZONE_ROWS: &[(&str, Option<&str>)] = &[
    ("us-west-2", Some("Z22EZDAJT6W257")),
    ("us-east-1", None),
    ("eu-central-1", None),
];

#[test]
fn every_graph_cluster_of_a_region_reports_that_regions_published_hosted_zone() {
    on_router("graph-cluster-hosted-zone", |router, runtime| {
        // Keyed by (door, region) so the checks after the table can compare the zones one
        // door reports across regions, and the two doors against each other.
        let mut reported: Vec<(&str, &str, String)> = Vec::new();

        for door in DOORS {
            for (region, published) in ZONE_ROWS {
                // TWO clusters per row, because the invariance is the point: one zone per
                // region, whatever the cluster.
                let mut zones: Vec<String> = Vec::new();
                for ordinal in ["a", "b"] {
                    let identifier = format!("emu-hz-{}-{region}-{ordinal}", door.key);
                    let case = format!("the {} door, {region}, cluster {ordinal}", door.key);

                    let created = ok(
                        &case,
                        "CreateDBCluster",
                        call(
                            router,
                            runtime,
                            region,
                            door.key,
                            "CreateDBCluster",
                            &[("DBClusterIdentifier", &identifier), ("Engine", ENGINE)],
                        ),
                    );
                    let zone = the_hosted_zone(&case, "CreateDBCluster", &created);
                    assert_well_formed(&case, &zone);
                    if let Some(expected) = published {
                        assert_eq!(
                            zone, *expected,
                            "case {case:?}: expected the published <HostedZoneId> \
                             {expected:?}, got {zone:?} in {created}"
                        );
                    }

                    // ROUND TRIP: the read-back reports the same zone, so a client that
                    // re-reads the cluster to build a Route 53 record gets one answer.
                    let described = ok(
                        &case,
                        "DescribeDBClusters",
                        call(
                            router,
                            runtime,
                            region,
                            door.key,
                            "DescribeDBClusters",
                            &[("DBClusterIdentifier", &identifier)],
                        ),
                    );
                    let read_back = the_hosted_zone(&case, "DescribeDBClusters", &described);
                    assert_eq!(
                        read_back, zone,
                        "case {case:?} (DescribeDBClusters): expected {zone:?}, got \
                         {read_back:?} in {described}"
                    );

                    if door.serves_modify {
                        // The third path that answers with a cluster, taken once the
                        // transition the create owes has settled.
                        poll_until_available(router, runtime, region, door.key, &case, &identifier);
                        let modified = ok(
                            &case,
                            "ModifyDBCluster",
                            call(
                                router,
                                runtime,
                                region,
                                door.key,
                                "ModifyDBCluster",
                                &[
                                    ("DBClusterIdentifier", &identifier),
                                    ("BackupRetentionPeriod", "7"),
                                ],
                            ),
                        );
                        let after_modify = the_hosted_zone(&case, "ModifyDBCluster", &modified);
                        assert_eq!(
                            after_modify, zone,
                            "case {case:?} (ModifyDBCluster): expected {zone:?}, got \
                             {after_modify:?} in {modified}"
                        );
                    }

                    zones.push(zone);
                }
                assert_eq!(
                    zones.first(),
                    zones.last(),
                    "case {:?}: every cluster of one engine in one region sits in ONE hosted \
                     zone, got {zones:?}",
                    format!("the {} door, {region}", door.key)
                );
                reported.push((
                    door.key,
                    region,
                    zones.into_iter().next().expect("two clusters were read"),
                ));
            }
        }

        // One zone per REGION: no two regions of one door share an id, so a client cannot
        // read a cluster's region out of another region's zone.
        for door in DOORS {
            let of_this_door: Vec<&(&str, &str, String)> = reported
                .iter()
                .filter(|(key, _, _)| *key == door.key)
                .collect();
            for (index, (_, region, zone)) in of_this_door.iter().enumerate() {
                for (_, other_region, other_zone) in of_this_door.iter().skip(index + 1) {
                    assert_ne!(
                        zone, other_zone,
                        "case {:?}: {region} and {other_region} are different regions and \
                         cannot report one hosted zone, both reported {zone:?}",
                        door.key
                    );
                }
            }
        }

        // The zone belongs to the region and the engine's endpoint domain, not to the door
        // the caller reached the control plane through: both doors report one id per region,
        // and the published one wherever AWS publishes it.
        for (region, published) in ZONE_ROWS {
            let of_this_region: Vec<&(&str, &str, String)> = reported
                .iter()
                .filter(|(_, reported_region, _)| reported_region == region)
                .collect();
            for (door, _, zone) in &of_this_region {
                let case = format!("the {door} door, {region}");
                if let Some(expected) = published {
                    assert_eq!(
                        zone, expected,
                        "case {case:?}: the published zone of {region} is {expected:?}, got \
                         {zone:?}"
                    );
                }
                assert_eq!(
                    Some(zone),
                    of_this_region.first().map(|(_, _, first)| first),
                    "case {case:?}: every door that serves this engine reports the one zone \
                     of {region}, got {of_this_region:?}"
                );
            }
        }
    });
}

#[test]
fn a_cluster_member_reports_the_hosted_zone_of_its_clusters_region() {
    on_router("graph-member-hosted-zone", |router, runtime| {
        // The region the published id is cited for, so the member's zone is pinned to a
        // value this suite states rather than to one the emulator computes.
        let (region, published) = ("us-west-2", "Z22EZDAJT6W257");

        for door in DOORS {
            let case = format!("the {} door, a member of a {region} cluster", door.key);
            let cluster = format!("emu-hz-member-{}", door.key);
            let member = format!("emu-hz-member-{}-one", door.key);

            let created = ok(
                &case,
                "CreateDBCluster",
                call(
                    router,
                    runtime,
                    region,
                    door.key,
                    "CreateDBCluster",
                    &[("DBClusterIdentifier", &cluster), ("Engine", ENGINE)],
                ),
            );
            let cluster_zone = the_hosted_zone(&case, "CreateDBCluster", &created);
            assert_eq!(
                cluster_zone, published,
                "case {case:?}: expected the published <HostedZoneId> {published:?}, got \
                 {cluster_zone:?} in {created}"
            );

            poll_until_available(router, runtime, region, door.key, &case, &cluster);
            let joined = ok(
                &case,
                "CreateDBInstance",
                call(
                    router,
                    runtime,
                    region,
                    door.key,
                    "CreateDBInstance",
                    &[
                        ("DBInstanceIdentifier", &member),
                        ("Engine", ENGINE),
                        ("DBInstanceClass", MEMBER_CLASS),
                        ("DBClusterIdentifier", &cluster),
                    ],
                ),
            );
            let member_zone = the_hosted_zone(&case, "CreateDBInstance", &joined);
            assert_eq!(
                member_zone, cluster_zone,
                "case {case:?}: an instance's endpoint sits in its region's zone \
                 {cluster_zone:?}, got {member_zone:?} in {joined}"
            );

            // ROUND TRIP: the read-back of the instance reports the same zone.
            let described = ok(
                &case,
                "DescribeDBInstances",
                call(
                    router,
                    runtime,
                    region,
                    door.key,
                    "DescribeDBInstances",
                    &[("DBInstanceIdentifier", &member)],
                ),
            );
            let read_back = the_hosted_zone(&case, "DescribeDBInstances", &described);
            assert_eq!(
                read_back, cluster_zone,
                "case {case:?} (DescribeDBInstances): expected {cluster_zone:?}, got \
                 {read_back:?} in {described}"
            );
        }
    });
}

/// Run `body` against a router carrying every service over one shared engine.
fn on_router(name: &str, body: impl FnOnce(&MultiServiceRouter, &Runtime) + Send + 'static) {
    let thread_name = name.to_string();
    let joined = std::thread::Builder::new()
        .name(thread_name)
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // The suite asserts service behaviour, not SigV4 verification; the credential
            // scope is read for routing only.
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // IO and timers are on because `CreateDBCluster` reaches a service that
                // opens a core listener for the cluster's own wire.
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            body(&router, &runtime);
        })
        .expect("spawn the suite's thread")
        .join();
    if let Err(panic) = joined {
        // Re-raise the suite's OWN panic, so the failure this test reports is the assertion
        // message that failed rather than "a thread panicked".
        std::panic::resume_unwind(panic);
    }
}

/// One awsQuery call, signed for `region` with the credential scope naming `door`.
fn call(
    router: &MultiServiceRouter,
    runtime: &Runtime,
    region: &str,
    door: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("host", "localhost")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{door}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, rendered)
}

/// Assert a 200 and hand back the body.
fn ok(case: &str, action: &str, response: (u16, String)) -> String {
    let (status, body) = response;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer HTTP 200, got {status} with body {body}"
    );
    body
}

/// The one `<HostedZoneId>` of a response that reports one resource.
///
/// Every call this suite makes names a single cluster or a single instance, so a body
/// carrying any other number of them is itself the failure.
fn the_hosted_zone(case: &str, action: &str, xml: &str) -> String {
    let found = elements(xml, "HostedZoneId");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: {action} must report exactly one <HostedZoneId>, got {found:?} in \
         {xml}"
    );
    element(xml, "HostedZoneId")
}

/// Assert `zone` is a well-formed Route 53 hosted zone id: the `Z` prefix every one of them
/// carries, followed by the digits-and-uppercase alphabet AWS writes them in.
fn assert_well_formed(case: &str, zone: &str) {
    assert!(
        zone.starts_with('Z')
            && zone.len() > 1
            && zone[1..]
                .chars()
                .all(|character| character.is_ascii_digit() || character.is_ascii_uppercase()),
        "case {case:?}: expected a Z-prefixed hosted zone id, got {zone:?}"
    );
}

/// Poll `DescribeDBClusters` until the cluster reports [`AVAILABLE`].
///
/// The read is a REPORTING call and nothing else, so only a transition the service's clock
/// owes can end the loop. The cluster's own `Status` is the first one a cluster report
/// carries; the statuses nested under it belong to other records.
fn poll_until_available(
    router: &MultiServiceRouter,
    runtime: &Runtime,
    region: &str,
    door: &str,
    case: &str,
    identifier: &str,
) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = ok(
            case,
            "DescribeDBClusters",
            call(
                router,
                runtime,
                region,
                door,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier)],
            ),
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        if status == AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock owed {identifier} the {AVAILABLE} state, last status was \
             {status:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}
