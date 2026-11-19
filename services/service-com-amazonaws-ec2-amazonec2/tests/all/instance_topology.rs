// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DescribeInstanceTopology`, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=DescribeInstanceTopology`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol"). ec2Query is selected by the `ec2` signing name in the request's SigV4
//! credential scope, so every call here carries one.
//!
//! Five rules are under test:
//!
//! 1. The report is COMPUTED FROM THE STORED INSTANCES of the Region the request was signed
//!    for: one row per instance, every member of it read off that instance, narrowed by the
//!    instance ids, the placement group names and the three documented filters the request
//!    names, and cut into `MaxResults`-sized pages.
//! 2. A row's `NetworkNodes` are the PATH THROUGH THE TOPOLOGY TREE the host the instance
//!    runs on hangs off: hashed per account, stable across reads, and shared between two
//!    instances exactly down to the level at which their placements stop agreeing.
//! 3. "The value is null or empty if … the instance is in a state other than `running`"
//!    (`InstanceTopology$NetworkNodes`): an instance that occupies no host is still reported,
//!    with its placement, and carries no path — while an instance whose TYPE has no topology
//!    ("instance topology is supported for specific instance types only") is not reported at
//!    all.
//! 4. A request outside a documented rule is REFUSED with EC2's own code for it.
//! 5. The report is a READ: repeated calls answer the same thing, and what every other reader
//!    of an instance reports is left alone.
//!
//! Every table row is a distinct input along what those rules generalise over — the instance
//! type and its family and generation, the zone, whether the instance is in a placement
//! group, the state it is parked in, which filter selects and with which wildcard, the page
//! size, and the Region the call is signed for — so no single recorded request is the
//! specification.
//!
//! Every response is read with the XML parser rather than by substring, so a member of one
//! row is never confused with an identically named member of another row's node set.

use std::time::{Duration, Instant};

use ec2::state::availability_zones_in_region;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, described_instance_state, error_code, filters, indexed};
use super::wire::{child_text, document, is_long_id, set_items};

/// The Region every call here is signed for unless the case says otherwise.
const REGION: &str = "us-west-2";

/// The Region the one instance that must NOT be reported is launched in: EC2 is reached
/// through a regional endpoint, so an instance of another Region is none of this one's.
const OTHER_REGION: &str = "eu-west-1";

/// The AMI every launch here names: `RunInstances` only needs an image id that resolves, and
/// which image an instance was launched from has no bearing on where it sits.
const AMI: &str = "ami-0123456789abcdef0";

/// The Availability Zones of [`REGION`] the launches place instances in. Each is resolved
/// against the Region's own zone catalog by [`zone_id_of`], which panics on a zone the Region
/// does not have, so these names are held to that catalog rather than merely written here.
///
/// The LAST zone of a Region offers no current-generation instance type
/// (`ec2::state::zone_offers_instance_type`) and every type topology covers is a current
/// generation one, so the instances with a topology are placed in the first two.
const ZONES: [&str; 3] = ["us-west-2a", "us-west-2b", "us-west-2c"];

/// The placement group two of the launches are placed in.
const GROUP: &str = "instance-topology-cluster";

/// How many levels the reported path through the topology tree has: the node of the
/// Availability Zone's network, the hosts of that zone serving the instance's type, and the
/// host it runs on.
const TOPOLOGY_DEPTH: usize = 3;

/// The prefix a topology node id carries.
const NODE_ID_PREFIX: &str = "nn";

/// How long a poll here keeps looking: a large multiple of the service's own transition span,
/// so a loaded or slow host is never the reason an assertion fails. Nothing is waited out —
/// the CONDITION is looked for again until it holds.
fn poll_deadline() -> Duration {
    ec2::state::TRANSITION_DELAY * 60
}

/// One look inside that deadline, so the deadline costs many looks rather than one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row of the selection table: what it covers, the form fields it sends, and the ids
/// those must select, in the order the report answers them.
type SelectionRow = (&'static str, Vec<(String, String)>, Vec<String>);

/// One row of the refusal table: what it covers, the form fields it sends, and the error code
/// and HTTP status the refusal must carry.
type RefusalRow = (&'static str, Vec<(String, String)>, &'static str, u16);

/// One form field.
fn p(name: &str, value: impl Into<String>) -> (String, String) {
    (name.to_string(), value.into())
}

/// One `DescribeInstanceTopology` call in [`REGION`].
async fn topology(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, "DescribeInstanceTopology", params).await
}

/// One `RunInstances` of a single instance signed for `region`, as the id it minted.
async fn launch_in(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    params: &[(String, String)],
) -> String {
    let mut all = vec![
        p("ImageId", AMI),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    all.extend_from_slice(params);
    let (status, body) = call(router, region, "RunInstances", &all).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    super::ec2_query::element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must report an instance: {body}"))
}

/// Look again until `instance_id` reports `wanted`, or a deadline far past the service's own
/// transition span passes: the launch, the stop and the terminate each owe their move to the
/// clock, so what is polled for is the STATE rather than a span of time.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let state = described_instance_state(router, REGION, instance_id).await;
        if state == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached {wanted:?}, last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One reported topology row, as the response wrote it. An absent optional member is `None`
/// and an empty one is `Some("")`, which is the difference an assertion on a member an
/// instance does not carry has to see.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    instance_id: String,
    instance_type: String,
    group_name: Option<String>,
    network_nodes: Vec<String>,
    availability_zone: String,
    zone_id: String,
    capacity_block_id: Option<String>,
}

/// The rows a successful response carries, in the order it reported them.
fn reported(case: &str, body: &str) -> Vec<Row> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "instanceSet")
        .into_iter()
        .map(|item| Row {
            instance_id: child_text(item, "instanceId").unwrap_or_default(),
            instance_type: child_text(item, "instanceType").unwrap_or_default(),
            group_name: child_text(item, "groupName"),
            network_nodes: set_items(item, "networkNodeSet")
                .into_iter()
                .map(|node| node.text().unwrap_or_default().to_string())
                .collect(),
            availability_zone: child_text(item, "availabilityZone").unwrap_or_default(),
            zone_id: child_text(item, "zoneId").unwrap_or_default(),
            capacity_block_id: child_text(item, "capacityBlockId"),
        })
        .collect()
}

/// The rows of a call that must succeed.
async fn reported_ok(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<Row> {
    let (status, body) = topology(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    reported(case, &body)
}

/// The ids a call that must succeed reported, in order.
async fn reported_ids(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> Vec<String> {
    reported_ok(router, case, params)
        .await
        .into_iter()
        .map(|row| row.instance_id)
        .collect()
}

/// The id of the Availability Zone named `zone_name`, read out of the Region's own zone
/// catalog (`ec2::state::availability_zones_in_region`) rather than written out here, so the
/// expectation follows the one definition of a zone's two spellings.
fn zone_id_of(region: &str, zone_name: &str) -> String {
    availability_zones_in_region(region)
        .into_iter()
        .find(|zone| zone.zone_name == zone_name)
        .map(|zone| zone.zone_id)
        .unwrap_or_else(|| panic!("{region} must have the Availability Zone {zone_name}"))
}

/// How many leading nodes two reported paths share — the depth at which the two placements
/// stop agreeing.
fn shared_nodes(left: &[String], right: &[String]) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

/// One launched instance of the table below: the launch it is made by, and what the topology
/// must then report for it.
struct Seed {
    case: &'static str,
    instance_type: &'static str,
    zone: &'static str,
    group: Option<&'static str>,
}

/// The instances rules 1 and 2 are asserted over.
///
/// The rows differ along everything those rules generalise over: the instance type, its family
/// and its size — entries of the modelled `InstanceType` catalog
/// (`smithy_query(query="shape",shape="InstanceType")`) drawn from four of the families AWS's
/// "Prerequisites for Amazon EC2 instance topology" page lists, accelerated and HPC — which
/// Availability Zone of the Region the launch placed the instance in, and whether it is in a
/// placement group. Two rows share a zone AND an instance type, two more share a zone, an
/// instance type and a placement group, and the rest sit apart, which is what makes the levels
/// of the tree observable.
fn seeds() -> Vec<Seed> {
    vec![
        Seed {
            case: "an accelerated type in the first zone",
            instance_type: "p4d.24xlarge",
            zone: ZONES[0],
            group: None,
        },
        Seed {
            case: "a second instance of that type and zone",
            instance_type: "p4d.24xlarge",
            zone: ZONES[0],
            group: None,
        },
        Seed {
            case: "an HPC type in the first zone",
            instance_type: "hpc7a.96xlarge",
            zone: ZONES[0],
            group: None,
        },
        Seed {
            case: "a Trainium type in a placement group in the second zone",
            instance_type: "trn1.32xlarge",
            zone: ZONES[1],
            group: Some(GROUP),
        },
        Seed {
            case: "a second instance of that group",
            instance_type: "trn1.32xlarge",
            zone: ZONES[1],
            group: Some(GROUP),
        },
        Seed {
            case: "a Graviton HPC type in the second zone",
            instance_type: "hpc7g.16xlarge",
            zone: ZONES[1],
            group: None,
        },
    ]
}

/// An emulator with [`GROUP`] created and every row of [`seeds`] launched and settled into
/// `running`, answered beside the ids in table order.
async fn seeded() -> (MultiServiceRouter, Vec<String>) {
    let (router, _engine) = super::ec2_query::emulator();
    let (status, body) = call(
        &router,
        REGION,
        "CreatePlacementGroup",
        &[p("GroupName", GROUP), p("Strategy", "cluster")],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreatePlacementGroup must succeed, got {status} with body {body}"
    );
    let mut ids = Vec::new();
    for seed in seeds() {
        let mut params = vec![
            p("InstanceType", seed.instance_type),
            p("Placement.AvailabilityZone", seed.zone),
        ];
        if let Some(group) = seed.group {
            params.push(p("Placement.GroupName", group));
        }
        ids.push(launch_in(&router, REGION, seed.case, &params).await);
    }
    for (seed, id) in seeds().iter().zip(ids.iter()) {
        poll_until_state(&router, seed.case, id, "running").await;
    }
    (router, ids)
}

/// Rule 1: one row per stored instance of the Region, every member of it read off that
/// instance — and rule 2's shape of a node id.
#[tokio::test]
async fn the_topology_reports_each_stored_instance_of_the_region() {
    let (router, ids) = seeded().await;
    let table = seeds();

    // An instance of ANOTHER Region: EC2 is regional, so it must not be reported here.
    let elsewhere = launch_in(
        &router,
        OTHER_REGION,
        "an instance of another Region",
        &[
            p("InstanceType", "p4d.24xlarge"),
            p("Placement.AvailabilityZone", "eu-west-1a"),
        ],
    )
    .await;

    let rows = reported_ok(&router, "every instance of the Region", &[]).await;
    let mut expected_ids = ids.clone();
    expected_ids.sort();
    let got_ids: Vec<String> = rows.iter().map(|row| row.instance_id.clone()).collect();
    assert_eq!(
        got_ids, expected_ids,
        "expected every instance of {REGION} in id order, got {got_ids:?}"
    );
    assert!(
        !got_ids.contains(&elsewhere),
        "expected the instance of another Region to be left out, got {got_ids:?}"
    );
    // Naming it by id answers the same: the ids select, so an id that names no instance of
    // THIS Region selects nothing rather than being refused.
    let named_elsewhere = reported_ids(
        &router,
        "the instance of another Region, named by id",
        &indexed("InstanceId", &[elsewhere.as_str()]),
    )
    .await;
    assert!(
        named_elsewhere.is_empty(),
        "expected no row for an instance of another Region, got {named_elsewhere:?}"
    );

    for (seed, id) in table.iter().zip(ids.iter()) {
        let case = seed.case;
        let row = rows
            .iter()
            .find(|row| &row.instance_id == id)
            .unwrap_or_else(|| panic!("case {case:?}: expected a row for {id}, got {rows:?}"));
        assert_eq!(
            row.instance_type, seed.instance_type,
            "case {case:?}: expected the instance's type, got {:?}",
            row.instance_type
        );
        assert_eq!(
            row.availability_zone, seed.zone,
            "case {case:?}: expected the instance's zone, got {:?}",
            row.availability_zone
        );
        assert_eq!(
            row.zone_id,
            zone_id_of(REGION, seed.zone),
            "case {case:?}: expected the zone's own id, got {:?}",
            row.zone_id
        );
        assert_eq!(
            row.group_name.as_deref(),
            seed.group,
            "case {case:?}: expected the placement group the launch named, got {:?}",
            row.group_name
        );
        // "This parameter is only supported for UltraServer instances"
        // (`InstanceTopology$CapacityBlockId`): no launch places an instance in a Capacity
        // Block, so no row carries one.
        assert_eq!(
            row.capacity_block_id, None,
            "case {case:?}: expected no capacity block, got {:?}",
            row.capacity_block_id
        );
        assert_eq!(
            row.network_nodes.len(),
            TOPOLOGY_DEPTH,
            "case {case:?}: expected {TOPOLOGY_DEPTH} network nodes, got {:?}",
            row.network_nodes
        );
        for node in &row.network_nodes {
            assert!(
                is_long_id(NODE_ID_PREFIX, node),
                "case {case:?}: expected a {NODE_ID_PREFIX}- node id, got {node:?}"
            );
        }
    }
}

/// Rule 2: two instances share exactly the levels at which their placements agree, and a read
/// answers the same node ids every time.
#[tokio::test]
async fn the_reported_nodes_are_shared_down_to_where_the_placements_agree() {
    let (router, ids) = seeded().await;
    let rows = reported_ok(&router, "the paths", &[]).await;
    let path_of = |index: usize| -> Vec<String> {
        let wanted = &ids[index];
        rows.iter()
            .find(|row| &row.instance_id == wanted)
            .map(|row| row.network_nodes.clone())
            .unwrap_or_else(|| panic!("expected a row for {wanted}, got {rows:?}"))
    };

    // Each pair: what it covers, the two seeds, and how many leading nodes they must share.
    let pairs: [(&str, usize, usize, usize); 6] = [
        ("the same zone and type, different hosts", 0, 1, 2),
        ("the same zone, different instance types", 0, 2, 1),
        ("the same zone, type and placement group", 3, 4, 3),
        ("different zones and types", 0, 3, 0),
        ("different zones, both ungrouped", 2, 5, 0),
        ("the same zone, one grouped and one not", 3, 5, 1),
    ];
    for (case, left, right, expected) in pairs {
        let (left_path, right_path) = (path_of(left), path_of(right));
        let shared = shared_nodes(&left_path, &right_path);
        assert_eq!(
            shared, expected,
            "case {case:?}: expected {expected} shared node(s) between {left_path:?} and \
             {right_path:?}, got {shared}"
        );
    }

    // Nothing is stored, so a second read must answer the same ids rather than fresh ones.
    let again = reported_ok(&router, "a second read of the paths", &[]).await;
    assert_eq!(
        again, rows,
        "expected a second read to report the same topology, got {again:?}"
    );
}

/// Rule 3: a path is reported for a `running` instance and for no other, and an instance that
/// has no path is still reported with its placement.
#[tokio::test]
async fn an_instance_that_is_not_running_is_reported_without_a_path() {
    let (router, _engine) = super::ec2_query::emulator();

    // Each row: what it covers, the action that parks the instance, and the state it ends in.
    let states: [(&str, Option<&str>, &str); 3] = [
        ("an instance that is running", None, "running"),
        ("an instance that has been stopped", Some("StopInstances"), "stopped"),
        (
            "an instance that has been terminated",
            Some("TerminateInstances"),
            "terminated",
        ),
    ];
    for (case, action, wanted) in states {
        let instance_id = launch_in(
            &router,
            REGION,
            case,
            &[
                p("InstanceType", "trn1.2xlarge"),
                p("Placement.AvailabilityZone", ZONES[0]),
            ],
        )
        .await;
        // Every instance is driven to `running` first, so the state under test is reached the
        // way a caller reaches it rather than by seeding a record.
        poll_until_state(&router, case, &instance_id, "running").await;
        if let Some(action) = action {
            let (status, body) = call(
                &router,
                REGION,
                action,
                &indexed("InstanceId", &[&instance_id]),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: {action} must succeed, got {status} with body {body}"
            );
            poll_until_state(&router, case, &instance_id, wanted).await;
        }

        let rows = reported_ok(&router, case, &indexed("InstanceId", &[&instance_id])).await;
        let row = rows
            .iter()
            .find(|row| row.instance_id == instance_id)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a row for {instance_id}, got {rows:?}")
            });
        // The placement is reported whatever the state: it is where the instance was placed,
        // not where it is running right now.
        assert_eq!(
            row.availability_zone, ZONES[0],
            "case {case:?}: expected the instance's zone, got {:?}",
            row.availability_zone
        );
        let expected_nodes = if wanted == "running" { TOPOLOGY_DEPTH } else { 0 };
        assert_eq!(
            row.network_nodes.len(),
            expected_nodes,
            "case {case:?}: an instance in state {wanted:?} must report {expected_nodes} \
             network node(s), got {:?}",
            row.network_nodes
        );
    }
}

/// Rule 1's type prerequisite: "instance topology is supported for specific instance types
/// only" (`DescribeInstanceTopology`), so an instance of a type AWS reports no topology for is
/// left out of the report entirely — while the account still holds it.
#[tokio::test]
async fn an_instance_of_an_unsupported_type_is_not_reported_at_all() {
    let (router, _engine) = super::ec2_query::emulator();

    // Each row: what it covers, the instance type, the zone it is placed in, and whether AWS
    // supports instance topology for that type. The supported rows are families from the
    // prerequisites page (accelerated and HPC) at sizes the seed table does not use; the
    // unsupported rows span burstable, general purpose, compute optimized, memory optimized
    // and a previous-generation family — and the last of them sits in the zone that offers no
    // current generation type, so the rule is about the TYPE and not about where it runs.
    let rows: [(&str, &str, &str, bool); 9] = [
        ("an accelerated type", "p5.48xlarge", ZONES[0], true),
        ("a second accelerated family", "p4de.24xlarge", ZONES[0], true),
        ("a Trainium type", "trn1n.32xlarge", ZONES[1], true),
        ("an HPC type", "hpc6a.48xlarge", ZONES[1], true),
        ("a burstable type", "t3.micro", ZONES[0], false),
        ("a general purpose type", "m5.4xlarge", ZONES[0], false),
        ("a compute optimized type", "c5.9xlarge", ZONES[1], false),
        ("a memory optimized type", "r6i.32xlarge", ZONES[1], false),
        ("a previous-generation type", "m3.medium", ZONES[2], false),
    ];
    let mut launched: Vec<(&str, String, bool)> = Vec::new();
    for (case, instance_type, zone, supported) in rows {
        let instance_id = launch_in(
            &router,
            REGION,
            case,
            &[
                p("InstanceType", instance_type),
                p("Placement.AvailabilityZone", zone),
            ],
        )
        .await;
        poll_until_state(&router, case, &instance_id, "running").await;
        launched.push((case, instance_id, supported));
    }

    let reported_all = reported_ids(&router, "every instance with a topology", &[]).await;
    for (case, instance_id, supported) in &launched {
        assert_eq!(
            reported_all.contains(instance_id),
            *supported,
            "case {case:?}: expected reported={supported} for {instance_id}, got \
             {reported_all:?}"
        );
        // The instance exists either way: the type decides whether it has a TOPOLOGY, not
        // whether the account holds it.
        let state = described_instance_state(&router, REGION, instance_id).await;
        assert_eq!(
            state, "running",
            "case {case:?}: {instance_id} must still be an instance of the account, got \
             {state:?}"
        );
        // Naming it by id answers the same: a resolvable id of an instance with no topology is
        // not a refusal, and not a row.
        let named = reported_ids(
            &router,
            case,
            &indexed("InstanceId", &[instance_id.as_str()]),
        )
        .await;
        let expected: Vec<String> = if *supported {
            vec![instance_id.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            named, expected,
            "case {case:?}: expected {expected:?} when the id is named, got {named:?}"
        );
    }
}

/// Rule 1's narrowing: the instance ids, the placement group names and every documented
/// filter select, and a selector nothing matches reports nothing.
#[tokio::test]
async fn the_topology_narrows_by_ids_by_group_names_and_by_every_documented_filter() {
    let (router, ids) = seeded().await;
    let expected = |indexes: &[usize]| -> Vec<String> {
        let mut wanted: Vec<String> = indexes.iter().map(|index| ids[*index].clone()).collect();
        wanted.sort();
        wanted
    };

    // Each row: what it covers, the form fields it sends, and the seeds it must select.
    let rows: Vec<SelectionRow> = vec![
        (
            "two instances named by id",
            indexed("InstanceId", &[ids[0].as_str(), ids[2].as_str()]),
            expected(&[0, 2]),
        ),
        (
            "the placement group both grouped instances are in",
            indexed("GroupName", &[GROUP]),
            expected(&[3, 4]),
        ),
        (
            "an id of the issued form the account holds no instance for",
            indexed("InstanceId", &["i-0000000000000dead"]),
            Vec::new(),
        ),
        (
            "a short-form id the account holds no instance for",
            indexed("InstanceId", &["i-12345678"]),
            Vec::new(),
        ),
        (
            "one instance of the Region beside an id that names none",
            indexed("InstanceId", &[ids[0].as_str(), "i-0000000000000dead"]),
            expected(&[0]),
        ),
        (
            "the zone by name",
            filters(&[("availability-zone", &[ZONES[0]])]),
            expected(&[0, 1, 2]),
        ),
        (
            "the zone by id",
            filters(&[("zone-id", &[zone_id_of(REGION, ZONES[1]).as_str()])]),
            expected(&[3, 4, 5]),
        ),
        (
            "an exact instance type",
            filters(&[("instance-type", &["hpc7a.96xlarge"])]),
            expected(&[2]),
        ),
        (
            "an instance family, with the documented * wildcard",
            filters(&[("instance-type", &["p4d*"])]),
            expected(&[0, 1]),
        ),
        (
            "an instance type with the documented ? wildcard",
            filters(&[("instance-type", &["trn1.32xlarg?"])]),
            expected(&[3, 4]),
        ),
        (
            "two filters, joined with AND",
            filters(&[
                ("availability-zone", &[ZONES[0]]),
                ("instance-type", &["p4d.24xlarge"]),
            ]),
            expected(&[0, 1]),
        ),
        (
            "two values of one filter, joined with OR",
            filters(&[("instance-type", &["hpc7a.96xlarge", "hpc7g.16xlarge"])]),
            expected(&[2, 5]),
        ),
        (
            "a zone and a type no instance combines",
            filters(&[
                ("availability-zone", &[ZONES[1]]),
                ("instance-type", &["p4d.24xlarge"]),
            ]),
            Vec::new(),
        ),
        (
            "a supported instance type no instance has",
            filters(&[("instance-type", &["p5.48xlarge"])]),
            Vec::new(),
        ),
    ];
    for (case, params, wanted) in rows {
        let got = reported_ids(&router, case, &params).await;
        assert_eq!(
            got, wanted,
            "case {case:?}: expected {wanted:?} in id order, got {got:?}"
        );
    }
}

/// Rule 1's paging: `MaxResults` bounds the page, the token resumes after it, and the walk
/// reports every instance exactly once.
#[tokio::test]
async fn the_topology_pages_the_store() {
    let (router, ids) = seeded().await;
    let mut expected_ids = ids.clone();
    expected_ids.sort();

    // Each page size is a distinct input along what the rule generalises over: one below the
    // store's size, one that divides it, and one above it.
    for page_size in [1_usize, 2, 10] {
        let case = format!("a walk with MaxResults={page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One page per instance at worst, and one more to read the token that ends the walk.
        for _ in 0..=ids.len() {
            let mut params = vec![p("MaxResults", page_size.to_string())];
            if let Some(token) = token.as_deref() {
                params.push(p("NextToken", token));
            }
            let (status, body) = topology(&router, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let page = reported(&case, &body);
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page must hold at most {page_size} row(s), got {:?}",
                page.len()
            );
            walked.extend(page.into_iter().map(|row| row.instance_id));
            token = super::ec2_query::member(&body, "nextToken");
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            token, None,
            "case {case:?}: the walk must end without a token, got {token:?}"
        );
        assert_eq!(
            walked, expected_ids,
            "case {case:?}: expected the whole store once, in id order, got {walked:?}"
        );
    }
}

/// Rule 4: a request outside a documented rule is refused with EC2's own code and status for
/// it, rather than served or silently narrowed.
#[tokio::test]
async fn the_topology_refuses_what_it_cannot_answer() {
    let (router, ids) = seeded().await;
    let too_many_ids: Vec<(String, String)> = (0..=100)
        .map(|index| p(&format!("InstanceId.{}", index + 1), ids[0].as_str()))
        .collect();

    // Each row: what it covers, the form fields it sends, and the code and status the refusal
    // must carry.
    let rows: Vec<RefusalRow> = vec![
        (
            "a filter name this operation does not document",
            filters(&[("instance-id", &[ids[0].as_str()])]),
            "InvalidParameterValue",
            400,
        ),
        (
            "a page size beside an explicit id selection",
            {
                let mut params = indexed("InstanceId", &[ids[0].as_str()]);
                params.push(p("MaxResults", "5"));
                params
            },
            "InvalidParameterCombination",
            400,
        ),
        (
            "more instance ids than the documented maximum",
            too_many_ids,
            "InvalidParameterValue",
            400,
        ),
        (
            "a pagination token this service did not mint",
            vec![p("NextToken", "not-a-token")],
            "InvalidPaginationToken",
            400,
        ),
        (
            "a placement group the account does not hold",
            indexed("GroupName", &["no-such-placement-group"]),
            "InvalidPlacementGroup.Unknown",
            400,
        ),
        (
            "a value that is not an instance id at all",
            indexed("InstanceId", &["i-not-an-instance"]),
            "InvalidInstanceID.Malformed",
            400,
        ),
        (
            "an instance id in no published form",
            indexed("InstanceId", &["i-123456"]),
            "InvalidInstanceID.Malformed",
            400,
        ),
        (
            "the all-zero placeholder no mint produces",
            indexed("InstanceId", &["i-00000000000000000"]),
            "InvalidInstanceID.Malformed",
            400,
        ),
        (
            "the permission probe, on a request that would have succeeded",
            vec![p("DryRun", "true")],
            "DryRunOperation",
            412,
        ),
    ];
    for (case, params, code, status) in rows {
        let (got_status, body) = topology(&router, &params).await;
        assert_eq!(
            got_status, status,
            "case {case:?}: expected HTTP {status}, got {got_status} with body {body}"
        );
        let got_code = error_code(&body);
        assert_eq!(
            got_code, code,
            "case {case:?}: expected the code {code:?}, got {got_code:?} in {body}"
        );
    }
}

/// Rule 5: reporting a topology is a READ — it changes neither what it reports next time nor
/// what any other reader of the same instances reports.
#[tokio::test]
async fn reading_the_topology_changes_nothing_a_later_caller_sees() {
    let (router, ids) = seeded().await;
    let before: Vec<String> = {
        let mut states = Vec::new();
        for id in &ids {
            states.push(described_instance_state(&router, REGION, id).await);
        }
        states
    };

    let first = reported_ok(&router, "the first read", &[]).await;
    // Several reads, including narrowed and paged ones, so nothing a selection does leaks
    // into what a later unnarrowed read answers.
    let _ = reported_ids(&router, "a narrowed read", &indexed("GroupName", &[GROUP])).await;
    let _ = reported_ids(&router, "a paged read", &[p("MaxResults", "1")]).await;
    let (dry_status, _) = topology(&router, &[p("DryRun", "true")]).await;
    assert_eq!(dry_status, 412, "the permission probe must answer 412");
    let again = reported_ok(&router, "the read after them", &[]).await;
    assert_eq!(
        again, first,
        "expected an unchanged topology, got {again:?}"
    );

    let after: Vec<String> = {
        let mut states = Vec::new();
        for id in &ids {
            states.push(described_instance_state(&router, REGION, id).await);
        }
        states
    };
    assert_eq!(
        after, before,
        "expected every instance to be left in the state it was in, got {after:?}"
    );
}
